use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::VecDeque;
use dashmap::DashMap;
use tracing::info;
use tokio::sync::{mpsc, Mutex};

use crate::core::protocol::batch_system::types::error::BatchError;

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

/// Оптимизированный криптопроцессор
pub struct OptimizedCryptoProcessor {
    // Каналы для worker'ов
    _worker_senders: Arc<Vec<mpsc::Sender<CryptoTask>>>,
    worker_receivers: Arc<Vec<Mutex<mpsc::Receiver<CryptoTask>>>>,

    // Результаты
    results: Arc<DashMap<u64, CryptoResult>>,

    // Статистика
    stats: Arc<DashMap<String, u64>>,

    // Управление
    is_running: Arc<std::sync::atomic::AtomicBool>,
    next_task_id: std::sync::atomic::AtomicU64,
}

impl OptimizedCryptoProcessor {
    pub fn new(num_workers: usize) -> Self {
        info!("🚀 Creating optimized crypto processor with {} workers", num_workers);

        let mut worker_senders = Vec::with_capacity(num_workers);
        let mut worker_receivers = Vec::with_capacity(num_workers);

        for _ in 0..num_workers {
            let (tx, rx) = mpsc::channel(1000);
            worker_senders.push(tx);
            worker_receivers.push(Mutex::new(rx));
        }

        let processor = Self {
            _worker_senders: Arc::new(worker_senders),
            worker_receivers: Arc::new(worker_receivers),
            results: Arc::new(DashMap::new()),
            stats: Arc::new(DashMap::new()),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            next_task_id: std::sync::atomic::AtomicU64::new(1),
        };

        processor.start_workers();

        processor
    }

    fn start_workers(&self) {
        let num_workers = self.worker_receivers.len();

        for worker_id in 0..num_workers {
            let worker_receivers = self.worker_receivers.clone();
            let results = self.results.clone();
            let stats = self.stats.clone();
            let is_running = self.is_running.clone();

            let worker_task = async move {
                Self::crypto_worker_loop(
                    worker_id,
                    worker_receivers,
                    results,
                    stats,
                    is_running,
                ).await;
            };

            tokio::spawn(worker_task);
        }

        info!("✅ Started {} crypto workers", num_workers);
    }

    async fn crypto_worker_loop(
        worker_id: usize,
        worker_receivers: Arc<Vec<Mutex<mpsc::Receiver<CryptoTask>>>>,
        results: Arc<DashMap<u64, CryptoResult>>,
        stats: Arc<DashMap<String, u64>>,
        is_running: Arc<std::sync::atomic::AtomicBool>,
    ) {
        info!("🔐 Crypto worker #{} started", worker_id);

        let mut processed = 0;
        let mut local_queue = VecDeque::new();

        while is_running.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::select! {
                // Берем из своей очереди
                task = async {
                    if let Some(receiver_mutex) = worker_receivers.get(worker_id) {
                        let mut receiver = receiver_mutex.lock().await;
                        receiver.recv().await
                    } else {
                        None
                    }
                } => {
                    if let Some(task) = task {
                        local_queue.push_back(task);
                    }
                }

                // Work-stealing
                _ = tokio::time::sleep(Duration::from_millis(5)) => {
                    let num_workers = worker_receivers.len();
                    for offset in 1..num_workers {
                        let victim_id = (worker_id + offset) % num_workers;

                        if let Some(victim_mutex) = worker_receivers.get(victim_id) {
                            let mut victim_receiver = victim_mutex.lock().await;
                            if let Ok(task) = victim_receiver.try_recv() {
                                local_queue.push_back(task);
                                *stats.entry("crypto_steals".to_string()).or_insert(0) += 1;
                                break;
                            }
                        }
                    }
                }
            }

            // Обработка задач
            while let Some(task) = local_queue.pop_front() {
                Self::process_crypto_task(
                    worker_id,
                    task,
                    &results,
                    &stats,
                    &mut processed,
                );
            }

            // Статистика
            if processed >= 50 {
                stats.insert(format!("crypto_worker_{}_processed", worker_id), processed as u64);
                processed = 0;
            }

            tokio::time::sleep(Duration::from_micros(5)).await;
        }

        info!("👋 Crypto worker #{} stopped", worker_id);
    }

    fn process_crypto_task(
        worker_id: usize,
        task: CryptoTask,
        results: &Arc<DashMap<u64, CryptoResult>>,
        stats: &Arc<DashMap<String, u64>>,
        processed: &mut u64,
    ) {
        let start_time = Instant::now();

        let result = match &task.operation {
            CryptoOperation::EncryptChaCha20 { key, nonce, plaintext } => {
                Self::encrypt_chacha20(key, nonce, plaintext)
            }
            CryptoOperation::DecryptChaCha20 { key, nonce, ciphertext } => {
                Self::decrypt_chacha20(key, nonce, ciphertext)
            }
            CryptoOperation::HashBlake3 { key, data } => {
                Self::hash_blake3(key, data)
            }
            CryptoOperation::DeriveKey { algorithm, input, context, output_len } => {
                Self::derive_key(algorithm, input, context, *output_len)
            }
        };

        let processing_time = start_time.elapsed();

        let crypto_result = CryptoResult {
            id: task.id,
            result,
            processing_time,
            worker_id,
        };

        results.insert(task.id, crypto_result);
        *processed += 1;

        // Статистика
        *stats.entry("crypto_tasks_processed".to_string()).or_insert(0) += 1;
    }

    fn encrypt_chacha20(
        key: &[u8; 32],
        nonce: &[u8; 12],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, String> {
        use chacha20::cipher::{KeyIvInit, StreamCipher};
        use chacha20::ChaCha20;

        let mut buffer = plaintext.to_vec();
        let mut cipher = ChaCha20::new(key.into(), nonce.into());
        cipher.apply_keystream(&mut buffer);

        Ok(buffer)
    }

    fn decrypt_chacha20(
        key: &[u8; 32],
        nonce: &[u8; 12],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, String> {
        Self::encrypt_chacha20(key, nonce, ciphertext)
    }

    fn hash_blake3(
        key: &[u8; 32],
        data: &[u8],
    ) -> Result<Vec<u8>, String> {
        use blake3::Hasher;

        let mut hasher = Hasher::new_keyed(key);
        hasher.update(data);
        let hash = hasher.finalize();

        Ok(hash.as_bytes().to_vec())
    }

    fn derive_key(
        algorithm: &KeyDerivationAlgorithm,
        input: &[u8],
        context: &[u8],
        output_len: usize,
    ) -> Result<Vec<u8>, String> {
        match algorithm {
            KeyDerivationAlgorithm::Blake3 => {
                use blake3::Hasher;

                let mut hasher = Hasher::new();
                hasher.update(input);
                hasher.update(context);

                let mut output = vec![0u8; output_len];
                hasher.finalize_xof().fill(&mut output);

                Ok(output)
            }
            KeyDerivationAlgorithm::HkdfSha256 => {
                use ring::hkdf;

                let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]);
                let prk = salt.extract(input);
                let context_slice = &[context];
                let okm = prk.expand(context_slice, hkdf::HKDF_SHA256)
                    .map_err(|e| format!("HKDF expand failed: {:?}", e))?;

                let mut output = vec![0u8; output_len];
                okm.fill(&mut output)
                    .map_err(|e| format!("HKDF fill failed: {:?}", e))?;

                Ok(output)
            }
            KeyDerivationAlgorithm::HkdfSha512 => {
                use ring::hkdf;

                let salt = hkdf::Salt::new(hkdf::HKDF_SHA512, &[]);
                let prk = salt.extract(input);
                let context_slice = &[context];
                let okm = prk.expand(context_slice, hkdf::HKDF_SHA512)
                    .map_err(|e| format!("HKDF expand failed: {:?}", e))?;

                let mut output = vec![0u8; output_len];
                okm.fill(&mut output)
                    .map_err(|e| format!("HKDF fill failed: {:?}", e))?;

                Ok(output)
            }
        }
    }

    /// Отправка криптозадачи
    pub async fn submit_crypto_task(&self, operation: CryptoOperation, session_id: Vec<u8>, priority: u8) -> Result<u64, BatchError> {
        let task_id = self.next_task_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let task = CryptoTask {
            id: task_id,
            operation,
            session_id,
            priority,
        };

        // Round-robin распределение
        let worker_idx = task_id as usize % self._worker_senders.len();

        match self._worker_senders[worker_idx].send(task).await {
            Ok(_) => {
                *self.stats.entry("crypto_tasks_submitted".to_string()).or_insert(0) += 1;
                Ok(task_id)
            }
            Err(e) => Err(BatchError::ProcessingError(format!("Failed to submit crypto task: {}", e))),
        }
    }

    /// Получение результата
    pub fn get_crypto_result(&self, task_id: u64) -> Option<CryptoResult> {
        self.results.get(&task_id).map(|r| r.clone())
    }

    /// Получение статистики
    pub fn get_stats(&self) -> std::collections::HashMap<String, u64> {
        let mut stats_map = std::collections::HashMap::new();

        for entry in self.stats.iter() {
            stats_map.insert(entry.key().clone(), *entry.value());
        }

        stats_map
    }

    /// Остановка
    pub async fn shutdown(&self) {
        info!("🛑 Shutting down crypto processor...");
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
        info!("✅ Crypto processor stopped");
    }
}

impl Drop for OptimizedCryptoProcessor {
    fn drop(&mut self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}