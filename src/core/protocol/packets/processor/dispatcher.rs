use std::sync::Arc;
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::Instant;
use tracing::{info, error, warn, trace};

// Заменяем старые импорты на фантомные
use crate::core::protocol::phantom_crypto::keys::PhantomSession;
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;

// Импорты для pipeline
use crate::core::protocol::packets::processor::pipeline::orchestrator::PipelineOrchestrator;
use crate::core::protocol::packets::processor::pipeline::stages::common::{PipelineContext};
use crate::core::protocol::packets::processor::pipeline::stages::decryption::PhantomDecryptionStage;
use crate::core::protocol::packets::processor::pipeline::stages::processing::PhantomProcessingStage;
use crate::core::protocol::packets::processor::pipeline::stages::encryption::PhantomEncryptionStage;
use super::priority::Priority;
use super::packet_service::PhantomPacketService;

pub struct Work {
    pub ctx: Arc<PhantomSession>,  // Заменяем SessionKeys на PhantomSession
    pub raw_payload: Vec<u8>,
    pub client_ip: SocketAddr,
    pub reply: oneshot::Sender<Vec<u8>>,
    pub received_at: Instant,
    pub priority: Priority,
    pub is_large: bool,
}

pub struct Dispatcher {
    tx: mpsc::Sender<Work>,
    phantom_crypto_pool: Arc<crate::core::protocol::crypto::crypto_pool_phantom::PhantomCryptoPool>,
}

impl Dispatcher {
    pub fn spawn(
        num_workers: usize,
        phantom_crypto_pool: Arc<crate::core::protocol::crypto::crypto_pool_phantom::PhantomCryptoPool>,
        phantom_packet_service: Arc<PhantomPacketService>,  // Добавляем сервис
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Work>(65536);
        let rx = Arc::new(Mutex::new(rx));

        for _ in 0..num_workers {
            let rx = Arc::clone(&rx);
            let phantom_crypto_pool = phantom_crypto_pool.clone();
            let phantom_packet_service = phantom_packet_service.clone();  // Клонируем сервис

            tokio::spawn(async move {
                let mut worker = DispatcherWorker::new(rx, phantom_crypto_pool, phantom_packet_service);
                worker.run().await;
            });
        }

        Dispatcher { tx, phantom_crypto_pool }
    }

    pub async fn process_directly(
        &self,
        ctx: Arc<PhantomSession>,
        packet_type: u8,
        payload: Vec<u8>,
        _client_ip: SocketAddr
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        // Используем фантомный пакетный процессор напрямую
        let processor = PhantomPacketProcessor::new();
        let result = processor.create_outgoing(&ctx, packet_type, &payload)?;
        Ok(result)
    }

    pub async fn submit(&self, work: Work) -> Result<(), mpsc::error::SendError<Work>> {
        self.tx.send(work).await
    }
}

struct DispatcherWorker {
    rx: Arc<Mutex<mpsc::Receiver<Work>>>,
    phantom_crypto_pool: Arc<crate::core::protocol::crypto::crypto_pool_phantom::PhantomCryptoPool>,
    phantom_packet_service: Arc<PhantomPacketService>,
}

impl DispatcherWorker {
    fn new(
        rx: Arc<Mutex<mpsc::Receiver<Work>>>,
        phantom_crypto_pool: Arc<crate::core::protocol::crypto::crypto_pool_phantom::PhantomCryptoPool>,
        phantom_packet_service: Arc<PhantomPacketService>
    ) -> Self {
        Self { rx, phantom_crypto_pool, phantom_packet_service }
    }

    async fn run(&mut self) {
        loop {
            let work = {
                let mut guard = self.rx.lock().await;
                guard.recv().await
            };

            if let Some(work) = work {
                self.process_work(work).await;
            } else {
                break;
            }
        }
    }

    async fn process_work(&self, work: Work) {
        let work_start = Instant::now();

        if work.reply.is_closed() {
            info!("Client disconnected, skipping processing");
            return;
        }

        // Получаем packet_type из сырых данных для ответа
        let response_packet_type = if work.raw_payload.len() >= 5 {
            work.raw_payload[4]
        } else {
            warn!("Packet too short, using default packet type for response");
            0x10 // Fallback to Test packet type
        };

        info!("Processing phantom work for {} (size: {} bytes, priority: {:?})",
              work.client_ip, work.raw_payload.len(), work.priority);

        // Создаем pipeline для обработки фантомного пакета
        let pipeline_start = Instant::now();
        // Создаем pipeline с PhantomPacketService
        let pipeline = PipelineOrchestrator::new()
            .add_stage(PhantomDecryptionStage::new(self.phantom_crypto_pool.clone()))
            .add_stage(PhantomProcessingStage::new(
                self.phantom_packet_service.clone(),
                work.client_ip
            ))
            .add_stage(PhantomEncryptionStage::new(
                response_packet_type,
                self.phantom_crypto_pool.clone()
            ));

        let pipeline_init_time = pipeline_start.elapsed();

        let context = PipelineContext::new(work.ctx, work.raw_payload);

        let execute_start = Instant::now();
        match pipeline.execute(context).await {
            Ok(encrypted_response) => {
                let execute_time = execute_start.elapsed();
                let total_work_time = work_start.elapsed();

                info!("Phantom pipeline execution - init: {:?}, execute: {:?}, total: {:?}, response size: {} bytes",
                      pipeline_init_time, execute_time, total_work_time, encrypted_response.len());

                let processing_time = Instant::now().duration_since(work.received_at);
                if processing_time.as_millis() > 100 {
                    warn!("Slow phantom packet processing: {}ms for {}", processing_time.as_millis(), work.client_ip);
                }

                let send_start = Instant::now();
                if let Err(e) = work.reply.send(encrypted_response) {
                    info!("Failed to send phantom response: {:?}", e);
                }
                let send_time = send_start.elapsed();

                trace!("Phantom response send time: {:?}", send_time);
            }
            Err(e) => {
                error!("Phantom pipeline processing failed: {}", e);
                let total_time = work_start.elapsed();
                warn!("Phantom pipeline failed after {:?}: {}", total_time, e);

                // Отправляем ошибку клиенту
                let error_response = format!("Phantom processing error: {}", e).into_bytes();
                let _ = work.reply.send(error_response);
            }
        }
    }
}