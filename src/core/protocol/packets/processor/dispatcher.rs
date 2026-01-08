use std::sync::Arc;
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::Instant;
use tracing::{info, error, warn, trace, debug};

use crate::core::protocol::phantom_crypto::core::keys::PhantomSession;

// Импорты для pipeline
use crate::core::protocol::packets::processor::pipeline::orchestrator::PipelineOrchestrator;
use crate::core::protocol::packets::processor::pipeline::stages::common::{PipelineContext};
use crate::core::protocol::packets::processor::pipeline::stages::decryption::PhantomDecryptionStage;
use crate::core::protocol::packets::processor::pipeline::stages::processing::PhantomProcessingStage;
use crate::core::protocol::packets::processor::pipeline::stages::encryption::PhantomEncryptionStage;
use crate::core::protocol::crypto::crypto_pool_phantom::PhantomCryptoPool;
use super::priority::Priority;
use super::packet_service::PhantomPacketService;

pub struct Work {
    pub ctx: Arc<PhantomSession>,
    pub raw_payload: Vec<u8>,
    pub client_ip: SocketAddr,
    pub reply: oneshot::Sender<Vec<u8>>,
    pub received_at: Instant,
    pub priority: Priority,
    pub is_large: bool,
}

pub struct Dispatcher {
    tx: mpsc::Sender<Work>,
    phantom_crypto_pool: Arc<PhantomCryptoPool>,
}

impl Dispatcher {
    pub fn spawn(
        num_workers: usize,
        phantom_crypto_pool: Arc<PhantomCryptoPool>,
        phantom_packet_service: Arc<PhantomPacketService>,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Work>(65536);
        let rx = Arc::new(Mutex::new(rx));

        info!("🚀 Starting Dispatcher with {} workers", num_workers);
        info!("  - Crypto pool: {}", phantom_crypto_pool.get_stats());
        info!("  - Max queue size: 65536");

        for worker_id in 0..num_workers {
            let rx = Arc::clone(&rx);
            let phantom_crypto_pool = Arc::clone(&phantom_crypto_pool);
            let phantom_packet_service = Arc::clone(&phantom_packet_service);

            tokio::spawn(async move {
                let mut worker = DispatcherWorker::new(
                    worker_id,
                    rx,
                    phantom_crypto_pool,
                    phantom_packet_service
                );
                worker.run().await;
            });
        }

        Dispatcher {
            tx,
            phantom_crypto_pool
        }
    }

    pub async fn process_directly(
        &self,
        ctx: Arc<PhantomSession>,
        packet_type: u8,
        payload: Vec<u8>,
        _client_ip: SocketAddr
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        // Используем криптопул для обработки напрямую
        debug!("Processing packet directly using crypto pool");
        match self.phantom_crypto_pool.encrypt(ctx, packet_type, payload).await {
            Ok(result) => Ok(result),
            Err(e) => Err(Box::new(e)),
        }
    }

    pub async fn submit(&self, work: Work) -> Result<(), mpsc::error::SendError<Work>> {
        let queue_size = self.get_queue_size().await;
        if queue_size > 50000 {
            warn!("Dispatcher queue is getting full: {} items", queue_size);
        }

        self.tx.send(work).await
    }

    pub async fn get_queue_size(&self) -> usize {
        // Здесь можно реализовать получение размера очереди
        // Для простоты возвращаем 0
        0
    }

    pub fn get_crypto_pool_stats(&self) -> String {
        self.phantom_crypto_pool.get_stats()
    }
}

struct DispatcherWorker {
    id: usize,
    rx: Arc<Mutex<mpsc::Receiver<Work>>>,
    phantom_crypto_pool: Arc<PhantomCryptoPool>,
    phantom_packet_service: Arc<PhantomPacketService>,
}

impl DispatcherWorker {
    fn new(
        id: usize,
        rx: Arc<Mutex<mpsc::Receiver<Work>>>,
        phantom_crypto_pool: Arc<PhantomCryptoPool>,
        phantom_packet_service: Arc<PhantomPacketService>
    ) -> Self {
        debug!("🔧 Creating DispatcherWorker id={}", id);
        Self {
            id,
            rx,
            phantom_crypto_pool,
            phantom_packet_service
        }
    }

    async fn run(&mut self) {
        info!("🚀 DispatcherWorker id={} started", self.id);

        let mut processed_count = 0;
        let start_time = Instant::now();

        loop {
            let work = {
                let mut guard = self.rx.lock().await;
                guard.recv().await
            };

            if let Some(work) = work {
                self.process_work(work).await;
                processed_count += 1;

                // Периодически логируем статистику
                if processed_count % 100 == 0 {
                    debug!("DispatcherWorker id={} processed {} packets in {:?}",
                           self.id, processed_count, start_time.elapsed());
                }
            } else {
                break;
            }
        }

        info!("🛑 DispatcherWorker id={} stopped after {:?}", self.id, start_time.elapsed());
        info!("  - Total packets processed: {}", processed_count);
        info!("  - Crypto pool stats: {}", self.phantom_crypto_pool.get_stats());
    }

    async fn process_work(&self, work: Work) {
        let work_start = Instant::now();

        if work.reply.is_closed() {
            debug!("Client disconnected, skipping processing (worker id={})", self.id);
            return;
        }

        // Получаем packet_type из сырых данных для ответа
        let response_packet_type = if work.raw_payload.len() >= 5 {
            work.raw_payload[4]
        } else {
            warn!("Packet too short, using default packet type for response (worker id={})", self.id);
            0x10 // Fallback to Test packet type
        };

        debug!("Processing phantom work for {} (size: {} bytes, priority: {:?}, worker id={})",
              work.client_ip, work.raw_payload.len(), work.priority, self.id);

        // Создаем pipeline для обработки фантомного пакета
        let pipeline_start = Instant::now();

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

                debug!("Phantom pipeline execution (worker id={}) - init: {:?}, execute: {:?}, total: {:?}, response size: {} bytes",
                      self.id, pipeline_init_time, execute_time, total_work_time, encrypted_response.len());

                let processing_time = Instant::now().duration_since(work.received_at);
                if processing_time.as_millis() > 100 {
                    warn!("Slow phantom packet processing: {}ms for {} (worker id={})",
                          processing_time.as_millis(), work.client_ip, self.id);
                }

                let send_start = Instant::now();
                if let Err(e) = work.reply.send(encrypted_response) {
                    debug!("Failed to send phantom response (worker id={}): {:?}", self.id, e);
                }
                let _send_time = send_start.elapsed();

                trace!("Phantom response sent (worker id={})", self.id);
            }
            Err(e) => {
                error!("Phantom pipeline processing failed (worker id={}): {}", self.id, e);
                let total_time = work_start.elapsed();
                warn!("Phantom pipeline failed after {:?} (worker id={}): {}", total_time, self.id, e);

                // Отправляем ошибку клиенту
                let error_response = format!("Phantom processing error: {}", e).into_bytes();
                let _ = work.reply.send(error_response);
            }
        }
    }
}