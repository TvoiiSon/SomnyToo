use std::sync::Arc;
use std::time::{Instant, Duration};
use tokio::sync::{mpsc, oneshot, Semaphore};
use tracing::{warn, debug, info};

use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;
use crate::core::protocol::error::{ProtocolResult, ProtocolError, CryptoError};
use crate::core::protocol::phantom_crypto::{
    core::keys::PhantomSession,
    runtime::runtime::PhantomRuntime,
    optimization::batch_processor::{PhantomBatch, PhantomBatchProcessor},
};

/// Полностью оптимизированный криптографический пул
pub struct PhantomCryptoPool {
    runtime: Arc<PhantomRuntime>,
    batch_processor: Arc<PhantomBatchProcessor>,
    task_tx: mpsc::Sender<CryptoTask>,
    batch_tx: mpsc::Sender<BatchTask>,
    concurrency_limiter: Arc<Semaphore>,
    packet_processor: Arc<PhantomPacketProcessor>,
}

enum CryptoTask {
    Decrypt {
        session: Arc<PhantomSession>,
        payload: Vec<u8>,
        resp: oneshot::Sender<ProtocolResult<(u8, Vec<u8>)>>,
    },
    Encrypt {
        session: Arc<PhantomSession>,
        packet_type: u8,
        plaintext: Vec<u8>,
        resp: oneshot::Sender<ProtocolResult<Vec<u8>>>,
    },
}

struct BatchTask {
    batch: PhantomBatch,
    resp: oneshot::Sender<ProtocolResult<BatchResult>>,
}

pub struct BatchResult {
    pub packet_types: Vec<u8>,
    pub data_sizes: Vec<usize>,
    pub errors: Vec<Option<ProtocolError>>,
}

impl PhantomCryptoPool {
    pub fn spawn(num_workers: usize) -> Self {
        let runtime = Arc::new(PhantomRuntime::new(num_workers));
        let batch_processor = runtime.batch_processor();
        let packet_processor = Arc::new(PhantomPacketProcessor::new());

        let (task_tx, task_rx) = mpsc::channel::<CryptoTask>(8192);
        let (batch_tx, batch_rx) = mpsc::channel::<BatchTask>(1024);

        let concurrency_limiter = Arc::new(Semaphore::new(num_workers * 2));

        // Создаем один worker вместо нескольких
        let worker = CryptoWorker::new(
            0,
            runtime.clone(),
            batch_processor.clone(),
            packet_processor.clone(),
            task_rx,
            batch_rx,
            concurrency_limiter.clone(),
        );

        // Запускаем worker
        tokio::spawn(async move {
            worker.run().await;
        });

        info!("✅ PhantomCryptoPool initialized with {} workers", num_workers);
        info!("  - Batch processor: ready");
        info!("  - Packet processor: ready");
        info!("  - Concurrency limit: {}", num_workers * 2);

        Self {
            runtime,
            batch_processor,
            task_tx,
            batch_tx,
            concurrency_limiter,
            packet_processor,
        }
    }

    #[inline]
    pub async fn decrypt(
        &self,
        session: Arc<PhantomSession>,
        payload: Vec<u8>,
    ) -> ProtocolResult<(u8, Vec<u8>)> {
        let _permit = self.concurrency_limiter.acquire().await.unwrap();
        let start = Instant::now();

        let (tx, rx) = oneshot::channel();

        let task = CryptoTask::Decrypt {
            session,
            payload,
            resp: tx,
        };

        if self.task_tx.send(task).await.is_err() {
            return Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Failed to send task".to_string()
                }
            });
        }

        match tokio::time::timeout(Duration::from_millis(10), rx).await {
            Ok(Ok(result)) => {
                debug!("Decryption completed in {:?}", start.elapsed());
                result
            }
            Ok(Err(_)) => Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Channel error".to_string()
                }
            }),
            Err(_) => {
                warn!("Decryption timeout");
                Err(ProtocolError::Timeout {
                    duration: Duration::from_millis(10)
                })
            }
        }
    }

    #[inline]
    pub async fn encrypt(
        &self,
        session: Arc<PhantomSession>,
        packet_type: u8,
        plaintext: Vec<u8>,
    ) -> ProtocolResult<Vec<u8>> {
        let _permit = self.concurrency_limiter.acquire().await.unwrap();

        let (tx, rx) = oneshot::channel();

        let task = CryptoTask::Encrypt {
            session,
            packet_type,
            plaintext,
            resp: tx,
        };

        if self.task_tx.send(task).await.is_err() {
            return Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Failed to send task".to_string()
                }
            });
        }

        match tokio::time::timeout(Duration::from_millis(5), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Channel error".to_string()
                }
            }),
            Err(_) => {
                warn!("Encryption timeout");
                Err(ProtocolError::Timeout {
                    duration: Duration::from_millis(5)
                })
            }
        }
    }

    pub async fn process_batch(
        &self,
        batch: PhantomBatch,
    ) -> ProtocolResult<BatchResult> {
        let start = Instant::now();

        if batch.len() == 0 {
            return Ok(BatchResult {
                packet_types: Vec::new(),
                data_sizes: Vec::new(),
                errors: Vec::new(),
            });
        }

        let (tx, rx) = oneshot::channel();

        let task = BatchTask {
            batch,
            resp: tx,
        };

        if self.batch_tx.send(task).await.is_err() {
            return Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Failed to send batch".to_string()
                }
            });
        }

        match tokio::time::timeout(Duration::from_millis(50), rx).await {
            Ok(Ok(result)) => {
                match result {
                    Ok(batch_result) => {
                        debug!("Batch of {} processed in {:?}",
                               batch_result.packet_types.len(), start.elapsed());
                        Ok(batch_result)
                    }
                    Err(e) => Err(e),
                }
            }
            Ok(Err(_)) => Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Channel error".to_string()
                }
            }),
            Err(_) => {
                warn!("Batch processing timeout");
                Err(ProtocolError::Timeout {
                    duration: Duration::from_millis(50)
                })
            }
        }
    }

    pub fn runtime(&self) -> &Arc<PhantomRuntime> {
        &self.runtime
    }

    // Добавляем методы для использования полей
    pub fn get_batch_processor(&self) -> &Arc<PhantomBatchProcessor> {
        &self.batch_processor
    }

    pub fn get_packet_processor(&self) -> &Arc<PhantomPacketProcessor> {
        &self.packet_processor
    }

    pub fn get_stats(&self) -> String {
        format!(
            "PhantomCryptoPool: runtime={:?}, batch_processor={:?}, packet_processor={:?}",
            self.runtime.get_performance_report(),
            "ready",
            "ready"
        )
    }
}

struct CryptoWorker {
    id: usize,
    runtime: Arc<PhantomRuntime>,
    batch_processor: Arc<PhantomBatchProcessor>,
    packet_processor: Arc<PhantomPacketProcessor>,
    task_rx: mpsc::Receiver<CryptoTask>,
    batch_rx: mpsc::Receiver<BatchTask>,
    concurrency_limiter: Arc<Semaphore>,
}

impl CryptoWorker {
    fn new(
        id: usize,
        runtime: Arc<PhantomRuntime>,
        batch_processor: Arc<PhantomBatchProcessor>,
        packet_processor: Arc<PhantomPacketProcessor>,
        task_rx: mpsc::Receiver<CryptoTask>,
        batch_rx: mpsc::Receiver<BatchTask>,
        concurrency_limiter: Arc<Semaphore>,
    ) -> Self {
        info!("🔧 Creating CryptoWorker id={}", id);
        info!("  - Runtime capabilities: {}", runtime.get_performance_report());
        info!("  - Batch processor ready: {}", !batch_processor.is_empty());

        Self {
            id,
            runtime,
            batch_processor,
            packet_processor,
            task_rx,
            batch_rx,
            concurrency_limiter,
        }
    }

    async fn run(mut self) {
        info!("🚀 CryptoWorker id={} started", self.id);

        let mut processed_tasks = 0;
        let mut processed_batches = 0;
        let start_time = Instant::now();

        loop {
            tokio::select! {
                Some(task) = self.task_rx.recv() => {
                    let _permit = self.concurrency_limiter.acquire().await.unwrap();
                    self.handle_task(task).await;
                    processed_tasks += 1;

                    // Периодически логируем статистику
                    if processed_tasks % 100 == 0 {
                        debug!("CryptoWorker id={} processed {} tasks in {:?}",
                               self.id, processed_tasks, start_time.elapsed());
                    }
                }
                Some(batch_task) = self.batch_rx.recv() => {
                    let _permit = self.concurrency_limiter.acquire().await.unwrap();
                    self.handle_batch(batch_task).await;
                    processed_batches += 1;

                    // Периодически логируем статистику
                    if processed_batches % 10 == 0 {
                        debug!("CryptoWorker id={} processed {} batches in {:?}",
                               self.id, processed_batches, start_time.elapsed());
                    }
                }
                else => break,
            }
        }

        info!("🛑 CryptoWorker id={} stopped after {:?}", self.id, start_time.elapsed());
        info!("  - Total tasks processed: {}", processed_tasks);
        info!("  - Total batches processed: {}", processed_batches);
        info!("  - Runtime stats: {}", self.runtime.get_performance_report());
    }

    async fn handle_task(&self, task: CryptoTask) {
        match task {
            CryptoTask::Decrypt { session, payload, resp } => {
                let result = self.packet_processor.process_incoming_vec(&payload, &session);
                let _ = resp.send(result);
            }
            CryptoTask::Encrypt { session, packet_type, plaintext, resp } => {
                let result = self.packet_processor.create_outgoing_vec(&session, packet_type, &plaintext);
                let _ = resp.send(result);
            }
        }
    }

    async fn handle_batch(&self, task: BatchTask) {
        let start = Instant::now();
        let batch_size = task.batch.len();

        let batch_result = self.batch_processor.process_batch(task.batch);

        let result = BatchResult {
            packet_types: batch_result.packet_types,
            data_sizes: batch_result.plaintexts.iter().map(|p| p.len()).collect(),
            errors: batch_result.errors,
        };

        let _ = task.resp.send(Ok(result));

        debug!("Batch processing completed in {:?} for {} items (worker id={})",
               start.elapsed(), batch_size, self.id);
    }
}