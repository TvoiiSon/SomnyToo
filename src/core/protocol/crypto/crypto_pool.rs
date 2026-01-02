use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{Instant, Duration};
use tracing::{info, error, warn, debug};
use aes_gcm::{
    aead::{Aead},
};
use rand_core::RngCore;

use crate::core::protocol::crypto::key_manager::session_keys::SessionKeys;
use crate::core::protocol::packets::decoder::packet_parser::PacketParser;

#[derive(Clone)]
pub struct CryptoPool {
    tx: mpsc::Sender<CryptoTask>,
    batch_tx: mpsc::Sender<CryptoBatchTask>,
}

pub enum CryptoTask {
    Single {
        ctx: Arc<SessionKeys>,
        payload: Vec<u8>,
        resp: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    Batch {
        tasks: Vec<(Arc<SessionKeys>, Vec<u8>)>,
        resp: oneshot::Sender<Result<Vec<Vec<u8>>, String>>,
    },
}

pub struct CryptoBatchTask {
    pub tasks: Vec<(Arc<SessionKeys>, Vec<u8>)>,
    pub resp: oneshot::Sender<Result<Vec<Vec<u8>>, String>>,
}

impl CryptoPool {
    pub fn spawn(threads: usize) -> Self {
        let (tx, rx) = mpsc::channel::<CryptoTask>(4096);
        let (batch_tx, batch_rx) = mpsc::channel::<CryptoBatchTask>(1024);

        let rx = Arc::new(Mutex::new(rx));
        let batch_rx = Arc::new(Mutex::new(batch_rx));

        // Основные воркеры
        for _ in 0..threads {
            let rx = Arc::clone(&rx);
            tokio::spawn(async move {
                let worker = CryptoWorker::new();
                worker.run(rx).await;
            });
        }

        // Batch воркеры
        for _ in 0..threads / 2 {
            let batch_rx = Arc::clone(&batch_rx);
            tokio::spawn(async move {
                let worker = CryptoWorker::new();
                worker.run_batch(batch_rx).await;
            });
        }

        CryptoPool { tx, batch_tx }
    }

    pub async fn decrypt(&self, ctx: &SessionKeys, payload: Vec<u8>) -> Result<Vec<u8>, String> {
        let (tx_resp, rx_resp) = oneshot::channel();
        let arc_ctx = Arc::new(ctx.clone());

        let task = CryptoTask::Single {
            ctx: arc_ctx,
            payload,
            resp: tx_resp,
        };

        if self.tx.send(task).await.is_err() {
            return Err("Failed to send decryption task".to_string());
        }

        match tokio::time::timeout(Duration::from_secs(3), rx_resp).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => {
                warn!("CryptoPool decrypt timeout");
                Err("Decryption timeout".to_string())
            }
        }
    }

    pub async fn encrypt(&self, ctx: Arc<SessionKeys>, plaintext: Vec<u8>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let start = Instant::now();
        info!("Encrypting payload of {} bytes for session {:?}", plaintext.len(), ctx.session_id);

        // Генерируем nonce
        let nonce_start = Instant::now();
        let nonce = self.generate_nonce();
        let nonce_time = nonce_start.elapsed();

        // Шифруем используя AEAD cipher из SessionKeys
        let encrypt_start = Instant::now();
        let ciphertext = ctx.aead_cipher
            .encrypt(&nonce.into(), plaintext.as_ref())
            .map_err(|e| format!("Encryption failed: {}", e))?;
        let encrypt_time = encrypt_start.elapsed();

        // Объединяем nonce и ciphertext
        let mut result = Vec::with_capacity(nonce.len() + ciphertext.len());
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&ciphertext);

        let total_time = start.elapsed();
        debug!("Encryption complete - nonce: {:?}, encrypt: {:?}, total: {:?}, output: {} bytes",
               nonce_time, encrypt_time, total_time, result.len());

        if total_time > Duration::from_millis(2) {
            info!("Slow encryption: {:?} for {} bytes", total_time, plaintext.len());
        }

        Ok(result)
    }

    fn generate_nonce(&self) -> [u8; 12] {
        let mut rng = rand::thread_rng();
        let mut nonce = [0u8; 12];
        rng.fill_bytes(&mut nonce);
        nonce
    }

    pub async fn encrypt_batch(&self, tasks: Vec<(Arc<SessionKeys>, Vec<u8>)>) -> Vec<Result<Vec<u8>, Box<dyn std::error::Error>>> {
        use futures::future::join_all;

        let futures = tasks.into_iter().map(|(ctx, plaintext)| {
            self.encrypt(ctx, plaintext)
        });

        join_all(futures).await
    }

    pub async fn decrypt_batch(&self, tasks: Vec<(Arc<SessionKeys>, Vec<u8>)>) -> Vec<Vec<u8>> {
        if tasks.is_empty() {
            return Vec::new();
        }

        let tasks_len = tasks.len();
        let (tx_resp, rx_resp) = oneshot::channel();

        if tasks_len <= 5 {
            let task = CryptoTask::Batch {
                tasks,
                resp: tx_resp,
            };

            if self.tx.send(task).await.is_err() {
                return vec![Vec::new(); tasks_len];
            }
        } else {
            let batch_task = CryptoBatchTask { tasks, resp: tx_resp };
            if self.batch_tx.send(batch_task).await.is_err() {
                return vec![Vec::new(); tasks_len];
            }
        }

        match tokio::time::timeout(Duration::from_secs(5), rx_resp).await {
            Ok(Ok(Ok(results))) => results,
            Ok(Ok(Err(e))) => {
                error!("Batch decryption failed: {}", e);
                vec![Vec::new(); tasks_len]
            }
            Ok(Err(_)) => {
                warn!("CryptoPool batch decrypt channel error");
                vec![Vec::new(); tasks_len]
            }
            Err(_) => {
                warn!("CryptoPool batch decrypt timeout");
                vec![Vec::new(); tasks_len]
            }
        }
    }
}

struct CryptoWorker;

impl CryptoWorker {
    fn new() -> Self {
        Self
    }

    async fn run(self, rx: Arc<Mutex<mpsc::Receiver<CryptoTask>>>) {
        loop {
            let task = {
                let mut guard = rx.lock().await;
                guard.recv().await
            };

            if let Some(task) = task {
                match task {
                    CryptoTask::Single { ctx, payload, resp } => {
                        Self::process_single(ctx, payload, resp).await;
                    }
                    CryptoTask::Batch { tasks, resp } => {
                        Self::process_batch(tasks, resp).await;
                    }
                }
            } else {
                break;
            }
        }
    }

    async fn run_batch(self, batch_rx: Arc<Mutex<mpsc::Receiver<CryptoBatchTask>>>) {
        loop {
            let batch_task = {
                let mut guard = batch_rx.lock().await;
                guard.recv().await
            };

            if let Some(batch_task) = batch_task {
                Self::process_batch_task(batch_task.tasks, batch_task.resp).await;
            } else {
                break;
            }
        }
    }

    async fn process_single(
        ctx: Arc<SessionKeys>,
        payload: Vec<u8>,
        resp: oneshot::Sender<Result<Vec<u8>, String>>
    ) {
        let start = Instant::now();
        let payload_size = payload.len();

        debug!("Decrypting packet for session {:?}, length: {}", ctx.session_id, payload_size);

        let result = match PacketParser::decode_packet(&ctx, &payload) {
            Ok((packet_type, data)) => {
                let decrypt_time = start.elapsed();
                info!("Successfully decrypted packet type: {:?}, data length: {}, time: {:?}",
                      packet_type, data.len(), decrypt_time);
                Ok(data)
            }
            Err(e) => {
                error!("Decryption failed for session {:?}: {}", ctx.session_id, e);
                Err(format!("Decryption failed: {}", e))
            }
        };

        let elapsed = start.elapsed();
        if elapsed > Duration::from_millis(5) {
            warn!("Slow decryption: {:?} for {} bytes", elapsed, payload_size);
        } else if elapsed > Duration::from_millis(1) {
            debug!("Decryption took {:?} for {} bytes", elapsed, payload_size);
        }

        let _ = resp.send(result);
    }

    async fn process_batch(
        tasks: Vec<(Arc<SessionKeys>, Vec<u8>)>,
        resp: oneshot::Sender<Result<Vec<Vec<u8>>, String>>
    ) {
        let batch_start = Instant::now();
        let batch_size = tasks.len();
        let mut results = Vec::new();
        let mut errors = Vec::new();

        info!("Processing batch of {} packets", batch_size);

        for (i, (ctx, payload)) in tasks.into_iter().enumerate() {
            let packet_start = Instant::now();
            match PacketParser::decode_packet(&ctx, &payload) {
                Ok((_packet_type, decrypted_data)) => {
                    let packet_time = packet_start.elapsed();
                    if packet_time > Duration::from_millis(5) {
                        debug!("Slow batch decryption [{}]: {:?} for {} bytes", i, packet_time, payload.len());
                    }
                    results.push(decrypted_data);
                }
                Err(e) => {
                    let packet_time = packet_start.elapsed();
                    error!("Batch decryption failed for session {:?} [{}]: {} (took {:?})",
                          ctx.session_id, i, e, packet_time);
                    errors.push(format!("Packet {}: {}", i, e));
                    results.push(Vec::new());
                }
            }
        }

        let batch_time = batch_start.elapsed();
        info!("Batch processing completed in {:?} for {} packets", batch_time, batch_size);

        let result = if errors.is_empty() {
            Ok(results)
        } else {
            Err(format!("Batch errors: {}", errors.join(", ")))
        };

        let _ = resp.send(result);
    }

    async fn process_batch_task(
        tasks: Vec<(Arc<SessionKeys>, Vec<u8>)>,
        resp: oneshot::Sender<Result<Vec<Vec<u8>>, String>>
    ) {
        Self::process_batch(tasks, resp).await;
    }
}