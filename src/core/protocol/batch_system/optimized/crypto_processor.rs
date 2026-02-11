use std::sync::Arc;
use std::time::{Instant, Duration};
use dashmap::DashMap;
use tracing::{info, debug};
use flume::{Sender, Receiver, bounded};

use crate::core::protocol::batch_system::types::error::BatchError;

// ИМПОРТЫ SIMD АКСЕЛЕРАТОРОВ
use crate::core::protocol::batch_system::acceleration_batch::chacha20_batch_accel::ChaCha20BatchAccelerator;
use crate::core::protocol::batch_system::acceleration_batch::blake3_batch_accel::Blake3BatchAccelerator;

/// Криптозадача
#[derive(Debug, Clone)]
pub struct CryptoTask {
    pub id: u64,
    pub operation: CryptoOperation,
    pub session_id: Vec<u8>,
    pub priority: u8,
}

/// Криптооперация
#[derive(Debug, Clone)]
pub enum CryptoOperation {
    EncryptChaCha20 {
        key: [u8; 32],
        nonce: [u8; 12],
        plaintext: Vec<u8>,
    },
    DecryptChaCha20 {
        key: [u8; 32],
        nonce: [u8; 12],
        ciphertext: Vec<u8>,
    },
    HashBlake3 {
        key: [u8; 32],
        data: Vec<u8>,
    },
    DeriveKey {
        algorithm: KeyDerivationAlgorithm,
        input: Vec<u8>,
        context: Vec<u8>,
        output_len: usize,
    },
}

#[derive(Debug, Clone)]
pub enum KeyDerivationAlgorithm {
    Blake3,
    HkdfSha256,
    HkdfSha512,
}

/// Крипторезультат
#[derive(Debug, Clone)]
pub struct CryptoResult {
    pub id: u64,
    pub result: Result<Vec<u8>, String>,
    pub processing_time: Duration,
    pub worker_id: usize,
}

/// Оптимизированный криптопроцессор с SIMD акселерацией
pub struct OptimizedCryptoProcessor {
    worker_senders: Arc<Vec<Sender<CryptoTask>>>,
    worker_receivers: Arc<Vec<Receiver<CryptoTask>>>,
    injector_sender: Sender<CryptoTask>,
    injector_receiver: Receiver<CryptoTask>,
    results: Arc<DashMap<u64, CryptoResult>>,
    stats: Arc<DashMap<String, u64>>,
    is_running: Arc<std::sync::atomic::AtomicBool>,
    next_task_id: std::sync::atomic::AtomicU64,

    // SIMD АКСЕЛЕРАТОРЫ
    chacha20_accelerator: Arc<ChaCha20BatchAccelerator>,
    blake3_accelerator: Arc<Blake3BatchAccelerator>,
}

impl OptimizedCryptoProcessor {
    pub fn new(num_workers: usize) -> Self {
        info!("🚀 Creating optimized crypto processor with {} workers and SIMD acceleration", num_workers);

        let mut worker_senders = Vec::with_capacity(num_workers);
        let mut worker_receivers = Vec::with_capacity(num_workers);

        for _ in 0..num_workers {
            let (tx, rx) = bounded(1000);
            worker_senders.push(tx);
            worker_receivers.push(rx);
        }

        let (injector_sender, injector_receiver) = bounded(2000);

        let chacha20_accelerator = Arc::new(ChaCha20BatchAccelerator::new(8));
        let blake3_accelerator = Arc::new(Blake3BatchAccelerator::new(8));

        let processor = Self {
            worker_senders: Arc::new(worker_senders),
            worker_receivers: Arc::new(worker_receivers),
            injector_sender,
            injector_receiver,
            results: Arc::new(DashMap::new()),
            stats: Arc::new(DashMap::new()),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            next_task_id: std::sync::atomic::AtomicU64::new(1),
            chacha20_accelerator,
            blake3_accelerator,
        };

        processor.start_workers();
        processor
    }

    fn start_workers(&self) {
        let num_workers = self.worker_receivers.len();

        for worker_id in 0..num_workers {
            let worker_receiver = self.worker_receivers[worker_id].clone();
            let injector_receiver = self.injector_receiver.clone();
            let results = self.results.clone();
            let stats = self.stats.clone();
            let is_running = self.is_running.clone();

            let chacha20 = self.chacha20_accelerator.clone();
            let blake3 = self.blake3_accelerator.clone();

            tokio::spawn(async move {
                Self::crypto_worker_loop(
                    worker_id,
                    worker_receiver,
                    injector_receiver,
                    results,
                    stats,
                    is_running,
                    chacha20,
                    blake3,
                ).await;
            });
        }

        info!("✅ Started {} crypto workers with SIMD acceleration", num_workers);
    }

    #[allow(clippy::too_many_arguments)]
    async fn crypto_worker_loop(
        worker_id: usize,
        worker_receiver: Receiver<CryptoTask>,
        injector_receiver: Receiver<CryptoTask>,
        results: Arc<DashMap<u64, CryptoResult>>,
        stats: Arc<DashMap<String, u64>>,
        is_running: Arc<std::sync::atomic::AtomicBool>,
        chacha20_accelerator: Arc<ChaCha20BatchAccelerator>,
        blake3_accelerator: Arc<Blake3BatchAccelerator>,
    ) {
        debug!("🔐 Crypto worker #{} started with SIMD", worker_id);

        let mut batch_buffer: Vec<CryptoTask> = Vec::with_capacity(32);
        let mut batch_start = Instant::now();

        while is_running.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::select! {
                Ok(task) = worker_receiver.recv_async() => {
                    batch_buffer.push(task);
                }
                Ok(task) = injector_receiver.recv_async() => {
                    stats.entry("crypto_steals".to_string())
                        .and_modify(|e| *e += 1)
                        .or_insert(1);
                    batch_buffer.push(task);
                }
                _ = tokio::time::sleep(Duration::from_micros(100)) => {
                    if !batch_buffer.is_empty() && batch_start.elapsed() > Duration::from_micros(500) {
                        Self::process_batch(
                            worker_id,
                            &mut batch_buffer,
                            &results,
                            &stats,
                            &chacha20_accelerator,
                            &blake3_accelerator,
                        ).await;
                        batch_buffer.clear();
                        batch_start = Instant::now();
                    }
                }
            }

            if batch_buffer.len() >= 16 {
                Self::process_batch(
                    worker_id,
                    &mut batch_buffer,
                    &results,
                    &stats,
                    &chacha20_accelerator,
                    &blake3_accelerator,
                ).await;
                batch_buffer.clear();
                batch_start = Instant::now();
            }
        }

        info!("👋 Crypto worker #{} stopped", worker_id);
    }

    async fn process_batch(
        worker_id: usize,
        tasks: &mut Vec<CryptoTask>,
        results: &Arc<DashMap<u64, CryptoResult>>,
        stats: &Arc<DashMap<String, u64>>,
        chacha20: &Arc<ChaCha20BatchAccelerator>,
        blake3: &Arc<Blake3BatchAccelerator>,
    ) {
        if tasks.is_empty() {
            return;
        }

        let start_time = Instant::now();
        let batch_size = tasks.len();

        let mut chacha_ops = Vec::new();
        let mut blake_ops = Vec::new();
        let mut derive_ops = Vec::new();

        for task in tasks.iter() {
            match &task.operation {
                CryptoOperation::EncryptChaCha20 { .. } |
                CryptoOperation::DecryptChaCha20 { .. } => chacha_ops.push(task),
                CryptoOperation::HashBlake3 { .. } => blake_ops.push(task),
                CryptoOperation::DeriveKey { .. } => derive_ops.push(task),
            }
        }

        if !chacha_ops.is_empty() {
            Self::process_chacha_batch(worker_id, &chacha_ops, results, stats, chacha20).await;
        }

        if !blake_ops.is_empty() {
            Self::process_blake_batch(worker_id, &blake_ops, results, stats, blake3).await;
        }

        for task in derive_ops {
            Self::process_derive_task(worker_id, task, results, stats).await;
        }

        let elapsed = start_time.elapsed();
        stats.entry("crypto_batch_processing_time".to_string())
            .and_modify(|e| *e += elapsed.as_micros() as u64)
            .or_insert(elapsed.as_micros() as u64);

        stats.entry("crypto_batches_processed".to_string())
            .and_modify(|e| *e += 1)
            .or_insert(1);

        stats.entry("crypto_tasks_processed".to_string())
            .and_modify(|e| *e += batch_size as u64)
            .or_insert(batch_size as u64);
    }

    async fn process_chacha_batch(
        worker_id: usize,
        tasks: &[&CryptoTask],
        results: &Arc<DashMap<u64, CryptoResult>>,
        stats: &Arc<DashMap<String, u64>>,
        accelerator: &Arc<ChaCha20BatchAccelerator>,
    ) {
        let batch_size = tasks.len();
        let mut keys = Vec::with_capacity(batch_size);
        let mut nonces = Vec::with_capacity(batch_size);
        let mut data_buffers = Vec::with_capacity(batch_size);
        let mut is_encryption = Vec::with_capacity(batch_size);

        for task in tasks {
            match &task.operation {
                CryptoOperation::EncryptChaCha20 { key, nonce, plaintext } => {
                    keys.push(*key);
                    nonces.push(*nonce);
                    data_buffers.push(plaintext.clone());
                    is_encryption.push(true);
                }
                CryptoOperation::DecryptChaCha20 { key, nonce, ciphertext } => {
                    keys.push(*key);
                    nonces.push(*nonce);
                    data_buffers.push(ciphertext.clone());
                    is_encryption.push(false);
                }
                _ => {}
            }
        }

        let processed = if is_encryption.iter().all(|&x| x) {
            accelerator.encrypt_batch(&keys, &nonces, &data_buffers).await
        } else {
            accelerator.decrypt_batch(&keys, &nonces, &data_buffers).await
        };

        for (i, task) in tasks.iter().enumerate() {
            let result = CryptoResult {
                id: task.id,
                result: Ok(processed[i].clone()),
                processing_time: Duration::from_micros(0),
                worker_id,
            };
            results.insert(task.id, result);
        }

        stats.entry("chacha_batch_operations".to_string())
            .and_modify(|e| *e += batch_size as u64)
            .or_insert(batch_size as u64);
    }

    async fn process_blake_batch(
        worker_id: usize,
        tasks: &[&CryptoTask],
        results: &Arc<DashMap<u64, CryptoResult>>,
        stats: &Arc<DashMap<String, u64>>,
        accelerator: &Arc<Blake3BatchAccelerator>,
    ) {
        let batch_size = tasks.len();
        let mut keys = Vec::with_capacity(batch_size);
        let mut inputs = Vec::with_capacity(batch_size);

        for task in tasks {
            if let CryptoOperation::HashBlake3 { key, data } = &task.operation {
                keys.push(*key);
                inputs.push(data.clone());
            }
        }

        let hashes = accelerator.hash_keyed_batch(&keys, &inputs).await;

        for (i, task) in tasks.iter().enumerate() {
            let result = CryptoResult {
                id: task.id,
                result: Ok(hashes[i].to_vec()),
                processing_time: Duration::from_micros(0),
                worker_id,
            };
            results.insert(task.id, result);
        }

        stats.entry("blake_batch_operations".to_string())
            .and_modify(|e| *e += batch_size as u64)
            .or_insert(batch_size as u64);
    }

    async fn process_derive_task(
        worker_id: usize,
        task: &CryptoTask,
        results: &Arc<DashMap<u64, CryptoResult>>,
        stats: &Arc<DashMap<String, u64>>,
    ) {
        if let CryptoOperation::DeriveKey { algorithm, input, context, output_len } = &task.operation {
            let result = match algorithm {
                KeyDerivationAlgorithm::Blake3 => {
                    use blake3::Hasher;
                    let mut hasher = Hasher::new();
                    hasher.update(input);
                    hasher.update(context);
                    let mut output = vec![0u8; *output_len];
                    hasher.finalize_xof().fill(&mut output);
                    Ok(output)
                }
                KeyDerivationAlgorithm::HkdfSha256 => {
                    use ring::hkdf;
                    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]);
                    let prk = salt.extract(input);
                    let context_slice = &[context.as_slice()];
                    match prk.expand(context_slice, hkdf::HKDF_SHA256) {
                        Ok(okm) => {
                            let mut output = vec![0u8; *output_len];
                            match okm.fill(&mut output) {
                                Ok(_) => Ok(output),
                                Err(e) => Err(format!("HKDF fill failed: {:?}", e)),
                            }
                        }
                        Err(e) => Err(format!("HKDF expand failed: {:?}", e)),
                    }
                }
                KeyDerivationAlgorithm::HkdfSha512 => {
                    use ring::hkdf;
                    let salt = hkdf::Salt::new(hkdf::HKDF_SHA512, &[]);
                    let prk = salt.extract(input);
                    let context_slice = &[context.as_slice()];
                    match prk.expand(context_slice, hkdf::HKDF_SHA512) {
                        Ok(okm) => {
                            let mut output = vec![0u8; *output_len];
                            match okm.fill(&mut output) {
                                Ok(_) => Ok(output),
                                Err(e) => Err(format!("HKDF fill failed: {:?}", e)),
                            }
                        }
                        Err(e) => Err(format!("HKDF expand failed: {:?}", e)),
                    }
                }
            };

            let crypto_result = CryptoResult {
                id: task.id,
                result: result.map_err(|e| e.to_string()),
                processing_time: Duration::from_micros(0),
                worker_id,
            };
            results.insert(task.id, crypto_result);

            stats.entry("derive_key_operations".to_string())
                .and_modify(|e| *e += 1)
                .or_insert(1);
        }
    }

    pub async fn submit_crypto_task(&self, operation: CryptoOperation, session_id: Vec<u8>, priority: u8) -> Result<u64, BatchError> {
        let task_id = self.next_task_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let task = CryptoTask {
            id: task_id,
            operation,
            session_id,
            priority,
        };

        let worker_idx = task_id as usize % self.worker_senders.len();

        match self.worker_senders[worker_idx].try_send(task.clone()) {
            Ok(_) => {
                self.stats.entry("crypto_tasks_submitted".to_string())
                    .and_modify(|e| *e += 1)
                    .or_insert(1);
                Ok(task_id)
            }
            Err(_) => {
                match self.injector_sender.try_send(task) {
                    Ok(_) => {
                        self.stats.entry("crypto_tasks_submitted".to_string())
                            .and_modify(|e| *e += 1)
                            .or_insert(1);
                        Ok(task_id)
                    }
                    Err(_) => Err(BatchError::ProcessingError("All crypto queues are full".to_string())),
                }
            }
        }
    }

    pub async fn shutdown(&self) {
        info!("🛑 Shutting down crypto processor...");
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
        info!("✅ Crypto processor stopped");
    }

    pub fn get_stats(&self) -> std::collections::HashMap<String, u64> {
        let mut stats_map = std::collections::HashMap::new();
        for entry in self.stats.iter() {
            stats_map.insert(entry.key().clone(), *entry.value());
        }
        stats_map
    }
}

impl Drop for OptimizedCryptoProcessor {
    fn drop(&mut self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}