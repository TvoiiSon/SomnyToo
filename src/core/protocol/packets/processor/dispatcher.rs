use std::sync::Arc;
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{Instant, Duration};
use tracing::{info, error, warn, debug};

use crate::core::protocol::phantom_crypto::core::keys::PhantomSession;
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
}

pub struct Dispatcher {
    tx: mpsc::Sender<Work>,
    phantom_crypto_pool: Arc<PhantomCryptoPool>,
    packet_service: Arc<PhantomPacketService>,
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
        info!("  - Packet service: ready");

        // Запускаем worker-ов
        for worker_id in 0..num_workers {
            let rx = Arc::clone(&rx);
            let phantom_crypto_pool = Arc::clone(&phantom_crypto_pool);
            let phantom_packet_service = Arc::clone(&phantom_packet_service);

            tokio::spawn(async move {
                let mut worker = DispatcherWorker::new(
                    worker_id,
                    rx,
                    phantom_crypto_pool,
                    phantom_packet_service,
                );
                worker.run().await;
            });
        }

        Dispatcher {
            tx,
            phantom_crypto_pool,
            packet_service: phantom_packet_service,
        }
    }

    pub async fn process_directly(
        &self,
        ctx: Arc<PhantomSession>,
        packet_type: u8,
        payload: Vec<u8>,
        _client_ip: SocketAddr
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        debug!("Processing packet directly using crypto pool");
        match self.phantom_crypto_pool.encrypt(ctx, packet_type, payload).await {
            Ok(result) => Ok(result),
            Err(e) => Err(Box::new(e)),
        }
    }

    pub async fn submit(&self, work: Work) -> Result<(), mpsc::error::SendError<Work>> {
        // Для всех пакетов - немедленная обработка
        self.tx.send(work).await
    }

    pub async fn get_queue_size(&self) -> usize {
        // Простая реализация
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
        phantom_packet_service: Arc<PhantomPacketService>,
    ) -> Self {
        debug!("🔧 Creating DispatcherWorker id={}", id);
        Self {
            id,
            rx,
            phantom_crypto_pool,
            phantom_packet_service,
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

        debug!("Processing phantom work for {} (size: {} bytes, priority: {:?}, worker id={})",
              work.client_ip, work.raw_payload.len(), work.priority, self.id);

        // Обрабатываем пакеты последовательно
        match self.phantom_crypto_pool.decrypt(work.ctx.clone(), work.raw_payload).await {
            Ok((packet_type, plaintext)) => {
                // Обрабатываем пакет
                match self.phantom_packet_service.process_packet(
                    work.ctx.clone(),
                    packet_type,
                    plaintext,
                    work.client_ip,
                ).await {
                    Ok(processing_result) => {
                        // Шифруем ответ
                        match self.phantom_crypto_pool.encrypt(
                            work.ctx,
                            processing_result.packet_type,
                            processing_result.response,
                        ).await {
                            Ok(encrypted_response) => {
                                let processing_time = Instant::now().duration_since(work.received_at);
                                if processing_time.as_millis() > 100 {
                                    warn!("Slow phantom packet processing: {}ms for {} (worker id={})",
                                          processing_time.as_millis(), work.client_ip, self.id);
                                }

                                if let Err(e) = work.reply.send(encrypted_response) {
                                    debug!("Failed to send phantom response (worker id={}): {:?}", self.id, e);
                                }
                            }
                            Err(e) => {
                                error!("Encryption failed (worker id={}): {}", self.id, e);
                                let error_msg = format!("Encryption error: {}", e).into_bytes();
                                let _ = work.reply.send(error_msg);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Packet processing failed (worker id={}): {}", self.id, e);
                        let error_msg = format!("Processing error: {}", e).into_bytes();
                        let _ = work.reply.send(error_msg);
                    }
                }
            }
            Err(e) => {
                error!("Decryption failed (worker id={}): {}", self.id, e);
                let error_msg = format!("Decryption error: {}", e).into_bytes();
                let _ = work.reply.send(error_msg);
            }
        }

        let total_time = work_start.elapsed();
        if total_time > Duration::from_millis(50) {
            warn!("Slow work processing: {:?} (worker id={})", total_time, self.id);
        }
    }
}