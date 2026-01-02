use std::sync::Arc;
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::Instant;
use tracing::{info, error, warn, trace};

use crate::core::protocol::crypto::key_manager::session_keys::SessionKeys;
use crate::core::protocol::packets::processor::packet_service::PacketService;
use crate::core::protocol::packets::processor::priority::Priority;

// Импорты для pipeline
use crate::core::protocol::packets::processor::pipeline::orchestrator::PipelineOrchestrator;
use crate::core::protocol::packets::processor::pipeline::stages::common::PipelineContext;
use crate::core::protocol::packets::processor::pipeline::stages::decryption::DecryptionStage;
use crate::core::protocol::packets::processor::pipeline::stages::processing::ProcessingStage;
use crate::core::protocol::packets::processor::pipeline::stages::encryption::EncryptionStage;

pub struct Work {
    pub ctx: Arc<SessionKeys>,
    pub raw_payload: Vec<u8>,
    pub client_ip: SocketAddr,
    pub reply: oneshot::Sender<Vec<u8>>,
    pub received_at: Instant,
    pub priority: Priority,
    pub is_large: bool,
}

pub struct Dispatcher {
    tx: mpsc::Sender<Work>,
    packet_service: PacketService,
}

impl Dispatcher {
    pub fn spawn(num_workers: usize, packet_service: PacketService) -> Self {
        let (tx, rx) = mpsc::channel::<Work>(65536);
        let rx = Arc::new(Mutex::new(rx));

        for _ in 0..num_workers {
            let rx = Arc::clone(&rx);
            let packet_service = packet_service.clone();

            tokio::spawn(async move {
                let mut worker = DispatcherWorker::new(rx, packet_service);
                worker.run().await;
            });
        }

        Dispatcher { tx, packet_service }
    }

    pub fn get_packet_service(&self) -> &PacketService {
        &self.packet_service
    }

    // Добавляем метод для обработки пакетов напрямую через сервис
    pub async fn process_directly(
        &self,
        ctx: Arc<SessionKeys>,
        packet_type: u8,
        payload: Vec<u8>,
        client_ip: SocketAddr
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let packet_type_enum = crate::core::protocol::packets::decoder::packet_parser::PacketType::from(packet_type);
        let result = self.packet_service.process_packet(ctx, packet_type_enum, payload, client_ip).await?;
        Ok(result.response)
    }

    pub async fn submit(&self, work: Work) -> Result<(), mpsc::error::SendError<Work>> {
        self.tx.send(work).await
    }
}

struct DispatcherWorker {
    rx: Arc<Mutex<mpsc::Receiver<Work>>>,
    packet_service: PacketService,
}

impl DispatcherWorker {
    fn new(rx: Arc<Mutex<mpsc::Receiver<Work>>>, packet_service: PacketService) -> Self {
        Self { rx, packet_service }
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

        info!("Processing work for {} (size: {} bytes, priority: {:?})",
          work.client_ip, work.raw_payload.len(), work.priority);

        // Создаем pipeline для обработки пакета
        let pipeline_start = Instant::now();
        let pipeline = PipelineOrchestrator::new()
            .add_stage(DecryptionStage)
            .add_stage(ProcessingStage::new(self.packet_service.clone(), work.client_ip))
            .add_stage(EncryptionStage::new(response_packet_type));

        let pipeline_init_time = pipeline_start.elapsed();

        let context = PipelineContext::new(work.ctx, work.raw_payload);

        let execute_start = Instant::now();
        match pipeline.execute(context).await {
            Ok(encrypted_response) => {
                let execute_time = execute_start.elapsed();
                let total_work_time = work_start.elapsed();

                info!("Pipeline execution - init: {:?}, execute: {:?}, total: {:?}, response size: {} bytes",
                  pipeline_init_time, execute_time, total_work_time, encrypted_response.len());

                let processing_time = Instant::now().duration_since(work.received_at);
                if processing_time.as_millis() > 100 {
                    warn!("Slow packet processing: {}ms for {}", processing_time.as_millis(), work.client_ip);
                }

                let send_start = Instant::now();
                if let Err(e) = work.reply.send(encrypted_response) {
                    info!("Failed to send response: {:?}", e);
                }
                let send_time = send_start.elapsed();

                trace!("Response send time: {:?}", send_time);
            }
            Err(e) => {
                error!("Pipeline processing failed: {}", e);
                let total_time = work_start.elapsed();
                warn!("Pipeline failed after {:?}: {}", total_time, e);

                // Отправляем ошибку клиенту
                let error_response = format!("Processing error: {}", e).into_bytes();
                let _ = work.reply.send(error_response);
            }
        }
    }
}
