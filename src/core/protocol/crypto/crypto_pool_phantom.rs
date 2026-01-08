use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{Instant, Duration};
use tracing::{info, error, warn, debug};

use crate::core::protocol::phantom_crypto::{
    keys::PhantomSession,
    packet::PhantomPacketProcessor,
};
use crate::core::protocol::error::{ProtocolResult, ProtocolError, CryptoError};

#[derive(Clone)]
pub struct PhantomCryptoPool {
    tx: mpsc::Sender<PhantomCryptoTask>,
    batch_tx: mpsc::Sender<PhantomBatchTask>,
    packet_processor: Arc<Mutex<PhantomPacketProcessor>>, // Теперь с Mutex для внутренних буферов
}

pub enum PhantomCryptoTask {
    Single {
        session: Arc<PhantomSession>,
        payload: Vec<u8>,
        resp: oneshot::Sender<ProtocolResult<(u8, Vec<u8>)>>,
    },
    Batch {
        tasks: Vec<(Arc<PhantomSession>, Vec<u8>)>,
        resp: oneshot::Sender<ProtocolResult<Vec<ProtocolResult<(u8, Vec<u8>)>>>>,
    },
}

pub struct PhantomBatchTask {
    pub tasks: Vec<(Arc<PhantomSession>, Vec<u8>)>,
    pub resp: oneshot::Sender<ProtocolResult<Vec<ProtocolResult<(u8, Vec<u8>)>>>>,
}

impl PhantomCryptoPool {
    pub fn spawn(threads: usize) -> Self {
        let (tx, rx) = mpsc::channel::<PhantomCryptoTask>(4096);
        let (batch_tx, batch_rx) = mpsc::channel::<PhantomBatchTask>(1024);

        // Создаем процессоры с предвыделенными буферами для каждого воркера
        let packet_processor = Arc::new(Mutex::new(PhantomPacketProcessor::new()));

        let rx = Arc::new(Mutex::new(rx));
        let batch_rx = Arc::new(Mutex::new(batch_rx));

        // Основные воркеры
        for _ in 0..threads {
            let rx = Arc::clone(&rx);
            let packet_processor = Arc::clone(&packet_processor);
            tokio::spawn(async move {
                let worker = PhantomCryptoWorker::new();
                worker.run(rx, packet_processor).await;
            });
        }

        // Batch воркеры
        for _ in 0..threads / 2 {
            let batch_rx = Arc::clone(&batch_rx);
            let packet_processor = Arc::clone(&packet_processor);
            tokio::spawn(async move {
                let worker = PhantomCryptoWorker::new();
                worker.run_batch(batch_rx, packet_processor).await;
            });
        }

        PhantomCryptoPool { tx, batch_tx, packet_processor }
    }

    pub async fn decrypt(
        &self,
        session: Arc<PhantomSession>,
        payload: Vec<u8>
    ) -> ProtocolResult<(u8, Vec<u8>)> {
        let (tx_resp, rx_resp) = oneshot::channel();

        let task = PhantomCryptoTask::Single {
            session,
            payload,
            resp: tx_resp,
        };

        if self.tx.send(task).await.is_err() {
            return Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Failed to send decryption task".to_string()
                }
            });
        }

        match tokio::time::timeout(Duration::from_secs(3), rx_resp).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Channel error".to_string()
                }
            }),
            Err(_) => {
                warn!("PhantomCryptoPool decrypt timeout");
                Err(ProtocolError::Timeout {
                    duration: Duration::from_secs(3)
                })
            }
        }
    }

    pub async fn encrypt(
        &self,
        session: Arc<PhantomSession>,
        packet_type: u8,
        plaintext: Vec<u8>
    ) -> ProtocolResult<Vec<u8>> {
        let start = Instant::now();

        info!(
            "Encrypting phantom payload: {} bytes, session: {}, type: 0x{:02X}",
            plaintext.len(),
            hex::encode(session.session_id()),
            packet_type
        );

        // Используем общий packet processor
        let mut processor = self.packet_processor.lock().await;
        let result = processor.create_outgoing(
            &session,
            packet_type,
            &plaintext,
        );

        let total_time = start.elapsed();
        debug!(
            "Phantom encryption complete in {:?}",
            total_time
        );

        if total_time > Duration::from_millis(2) {
            info!("Slow phantom encryption: {:?} for {} bytes", total_time, plaintext.len());
        }

        result.map(|slice| slice.to_vec()) // Копируем в Vec для совместимости
    }

    pub async fn encrypt_batch(
        &self,
        tasks: Vec<(Arc<PhantomSession>, u8, Vec<u8>)>
    ) -> Vec<ProtocolResult<Vec<u8>>> {
        use futures::future::join_all;

        let futures = tasks.into_iter().map(|(session, packet_type, plaintext)| {
            self.encrypt(session, packet_type, plaintext)
        });

        join_all(futures).await
    }

    pub async fn decrypt_batch(
        &self,
        tasks: Vec<(Arc<PhantomSession>, Vec<u8>)>
    ) -> Vec<ProtocolResult<(u8, Vec<u8>)>> {
        if tasks.is_empty() {
            return Vec::new();
        }

        let tasks_len = tasks.len();
        let (tx_resp, rx_resp) = oneshot::channel();

        if tasks_len <= 5 {
            let task = PhantomCryptoTask::Batch {
                tasks,
                resp: tx_resp,
            };

            if self.tx.send(task).await.is_err() {
                return create_error_results(
                    tasks_len,
                    ProtocolError::Crypto {
                        source: CryptoError::DecryptionFailed {
                            reason: "Failed to send batch task".to_string()
                        }
                    }
                );
            }
        } else {
            let batch_task = PhantomBatchTask { tasks, resp: tx_resp };
            if self.batch_tx.send(batch_task).await.is_err() {
                return create_error_results(
                    tasks_len,
                    ProtocolError::Crypto {
                        source: CryptoError::DecryptionFailed {
                            reason: "Failed to send batch task".to_string()
                        }
                    }
                );
            }
        }

        match tokio::time::timeout(Duration::from_secs(5), rx_resp).await {
            Ok(Ok(Ok(results))) => results,
            Ok(Ok(Err(e))) => {
                error!("Batch decryption failed: {}", e);
                create_error_results(tasks_len, e)
            }
            Ok(Err(_)) => {
                warn!("PhantomCryptoPool batch decrypt channel error");
                create_error_results(
                    tasks_len,
                    ProtocolError::Crypto {
                        source: CryptoError::DecryptionFailed {
                            reason: "Channel error".to_string()
                        }
                    }
                )
            }
            Err(_) => {
                warn!("PhantomCryptoPool batch decrypt timeout");
                create_error_results(
                    tasks_len,
                    ProtocolError::Timeout {
                        duration: Duration::from_secs(5)
                    }
                )
            }
        }
    }
}

/// Создает вектор результатов с одинаковой ошибкой
fn create_error_results(
    count: usize,
    error: ProtocolError
) -> Vec<ProtocolResult<(u8, Vec<u8>)>> {
    let mut results = Vec::with_capacity(count);
    for _ in 0..count {
        results.push(Err(error.clone()));
    }
    results
}

struct PhantomCryptoWorker;

impl PhantomCryptoWorker {
    fn new() -> Self {
        Self
    }

    async fn run(self, rx: Arc<Mutex<mpsc::Receiver<PhantomCryptoTask>>>,
                 packet_processor: Arc<Mutex<PhantomPacketProcessor>>) {
        loop {
            let task = {
                let mut guard = rx.lock().await;
                guard.recv().await
            };

            if let Some(task) = task {
                match task {
                    PhantomCryptoTask::Single { session, payload, resp } => {
                        Self::process_single(&packet_processor, session, payload, resp).await;
                    }
                    PhantomCryptoTask::Batch { tasks, resp } => {
                        Self::process_batch(&packet_processor, tasks, resp).await;
                    }
                }
            } else {
                break;
            }
        }
    }

    async fn run_batch(self, batch_rx: Arc<Mutex<mpsc::Receiver<PhantomBatchTask>>>,
                       packet_processor: Arc<Mutex<PhantomPacketProcessor>>) {
        loop {
            let batch_task = {
                let mut guard = batch_rx.lock().await;
                guard.recv().await
            };

            if let Some(batch_task) = batch_task {
                Self::process_batch_task(&packet_processor, batch_task.tasks, batch_task.resp).await;
            } else {
                break;
            }
        }
    }

    async fn process_single(
        packet_processor: &Arc<Mutex<PhantomPacketProcessor>>,
        session: Arc<PhantomSession>,
        payload: Vec<u8>,
        resp: oneshot::Sender<ProtocolResult<(u8, Vec<u8>)>>
    ) {
        let start = Instant::now();
        let payload_size = payload.len();

        debug!(
            "Decrypting phantom packet for session: {}, length: {}",
            hex::encode(session.session_id()),
            payload_size
        );

        let mut processor = packet_processor.lock().await;
        let result = processor.process_incoming(&payload, &session);

        let elapsed = start.elapsed();
        match &result {
            Ok((packet_type, data)) => {
                info!(
                    "Successfully decrypted phantom packet type: 0x{:02X}, data length: {}, time: {:?}",
                    packet_type,
                    data.len(),
                    elapsed
                );
                let _ = resp.send(Ok((*packet_type, data.to_vec())));
            }
            Err(e) => {
                error!(
                    "Phantom decryption failed for session {}: {}",
                    hex::encode(session.session_id()),
                    e
                );
                let _ = resp.send(Err(e.clone()));
            }
        };

        if elapsed > Duration::from_millis(5) {
            warn!("Slow phantom decryption: {:?} for {} bytes", elapsed, payload_size);
        } else if elapsed > Duration::from_millis(1) {
            debug!("Phantom decryption took {:?} for {} bytes", elapsed, payload_size);
        }
    }

    async fn process_batch(
        packet_processor: &Arc<Mutex<PhantomPacketProcessor>>,
        tasks: Vec<(Arc<PhantomSession>, Vec<u8>)>,
        resp: oneshot::Sender<ProtocolResult<Vec<ProtocolResult<(u8, Vec<u8>)>>>>
    ) {
        let batch_start = Instant::now();
        let batch_size = tasks.len();
        let mut results = Vec::with_capacity(batch_size);

        info!("Processing phantom batch of {} packets", batch_size);

        let mut processor = packet_processor.lock().await;

        for (i, (session, payload)) in tasks.into_iter().enumerate() {
            let packet_start = Instant::now();

            let result = processor.process_incoming(&payload, &session)
                .map(|(packet_type, data)| (packet_type, data.to_vec()));

            let packet_time = packet_start.elapsed();
            if packet_time > Duration::from_millis(5) {
                debug!(
                    "Slow phantom batch decryption [{}]: {:?} for {} bytes",
                    i, packet_time, payload.len()
                );
            }
            results.push(result);
        }

        let batch_time = batch_start.elapsed();
        info!(
            "Phantom batch processing completed in {:?} for {} packets",
            batch_time, batch_size
        );

        let _ = resp.send(Ok(results));
    }

    async fn process_batch_task(
        packet_processor: &Arc<Mutex<PhantomPacketProcessor>>,
        tasks: Vec<(Arc<PhantomSession>, Vec<u8>)>,
        resp: oneshot::Sender<ProtocolResult<Vec<ProtocolResult<(u8, Vec<u8>)>>>>
    ) {
        Self::process_batch(packet_processor, tasks, resp).await;
    }
}