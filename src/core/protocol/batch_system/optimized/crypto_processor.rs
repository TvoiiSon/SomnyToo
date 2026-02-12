use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::{VecDeque, HashMap};
use dashmap::DashMap;
use tracing::{info, debug};
use flume::{Sender, Receiver, bounded};
use tokio::sync::{Mutex, RwLock};
use crate::core::protocol::batch_system::acceleration_batch::chacha20_batch_accel::ChaCha20BatchAccelerator;
use crate::core::protocol::batch_system::acceleration_batch::blake3_batch_accel::Blake3BatchAccelerator;

/// Модель времени выполнения криптоопераций
/// T(n) = α + β·n + γ·SIMD(n) + δ·cache_miss(n)
#[derive(Debug, Clone, Copy)]
pub struct CryptoPerformanceModel {
    /// Фиксированные накладные расходы (α)
    pub alpha: f64,

    /// Линейный коэффициент на байт (β) - нс/байт
    pub beta: f64,

    /// Коэффициент эффективности SIMD (γ)
    pub gamma: f64,

    /// Штраф за кэш-промах (δ)
    pub delta: f64,

    /// Размер L1 кэша
    pub l1_cache_size: usize,

    /// Размер L2 кэша
    pub l2_cache_size: usize,

    /// Размер L3 кэша
    pub l3_cache_size: usize,

    /// Оптимальный размер для SIMD
    pub simd_optimal: usize,

    /// Максимальная пропускная способность (ops/s)
    pub max_throughput: f64,
}

impl CryptoPerformanceModel {
    pub fn new() -> Self {
        Self {
            alpha: 100.0,        // 100 нс накладных
            beta: 0.5,           // 0.5 нс/байт
            gamma: 0.3,          // 30% эффективность SIMD
            delta: 50.0,         // 50 нс штраф за кэш-промах
            l1_cache_size: 32 * 1024,     // 32 KB
            l2_cache_size: 256 * 1024,    // 256 KB
            l3_cache_size: 8 * 1024 * 1024, // 8 MB
            simd_optimal: 1024,  // 1KB оптимально для SIMD
            max_throughput: 1000000.0,     // 1M ops/s
        }
    }

    /// Время выполнения операции размера n
    pub fn execution_time(&self, size: usize, use_simd: bool) -> f64 {
        let n = size as f64;
        let mut time = self.alpha + self.beta * n;

        // SIMD ускорение
        if use_simd {
            let simd_factor = 1.0 - self.gamma * (n / self.simd_optimal as f64).min(1.0);
            time *= simd_factor;
        }

        // Штраф за кэш-промахи
        if size > self.l1_cache_size {
            time += self.delta * (size as f64 / self.l1_cache_size as f64).ln();
        }
        if size > self.l2_cache_size {
            time += self.delta * 0.5 * (size as f64 / self.l2_cache_size as f64).ln();
        }
        if size > self.l3_cache_size {
            time += self.delta * 0.25 * (size as f64 / self.l3_cache_size as f64).ln();
        }

        time
    }
}

#[derive(Debug, Clone)]
pub struct CryptoWorkStealingModel {
    /// Количество воркеров
    pub num_workers: usize,

    /// Вероятность кражи
    pub steal_probability: f64,

    /// Порог для кражи
    pub steal_threshold: usize,

    /// Коэффициент дисбаланса
    pub imbalance: f64,

    /// Эффективность кражи
    pub steal_efficiency: f64,
}

impl CryptoWorkStealingModel {
    pub fn new(num_workers: usize) -> Self {
        Self {
            num_workers,
            steal_probability: 0.1,
            steal_threshold: 10,
            imbalance: 0.0,
            steal_efficiency: 0.8,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BatchProcessingModel {
    /// Текущий размер батча
    pub current_batch_size: usize,

    /// Оптимальный размер батча
    pub optimal_batch_size: usize,

    /// Время накопления батча
    pub accumulation_time: Duration,

    /// Время обработки батча
    pub processing_time: Duration,

    /// Целевая задержка
    pub target_latency: Duration,

    /// Исторические данные
    pub history: VecDeque<BatchRecord>,
}

#[derive(Debug, Clone)]
pub struct BatchRecord {
    pub batch_size: usize,
    pub processing_time: Duration,
    pub throughput: f64,
    pub timestamp: Instant,
}

impl BatchProcessingModel {
    pub fn new(target_latency: Duration) -> Self {
        Self {
            current_batch_size: 32,
            optimal_batch_size: 32,
            accumulation_time: Duration::from_micros(0),
            processing_time: Duration::from_micros(0),
            target_latency,
            history: VecDeque::with_capacity(100),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CryptoTask {
    pub id: u64,
    pub operation: CryptoOperation,
    pub session_id: Vec<u8>,
    pub priority: u8,
    pub created_at: Instant,
    pub size_bytes: usize,
    pub estimated_time_ns: f64,
}

impl CryptoTask {
    pub fn new(id: u64, operation: CryptoOperation, session_id: Vec<u8>, priority: u8) -> Self {
        let size_bytes = operation.size_bytes();
        Self {
            id,
            operation,
            session_id,
            priority,
            created_at: Instant::now(),
            size_bytes,
            estimated_time_ns: 0.0,
        }
    }

    pub fn estimate_time(&mut self, model: &CryptoPerformanceModel) {
        let use_simd = matches!(self.operation,
            CryptoOperation::EncryptChaCha20 { .. } |
            CryptoOperation::DecryptChaCha20 { .. } |
            CryptoOperation::HashBlake3 { .. }
        );

        self.estimated_time_ns = model.execution_time(self.size_bytes, use_simd);
    }
}

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

impl CryptoOperation {
    pub fn size_bytes(&self) -> usize {
        match self {
            CryptoOperation::EncryptChaCha20 { plaintext, .. } => plaintext.len(),
            CryptoOperation::DecryptChaCha20 { ciphertext, .. } => ciphertext.len(),
            CryptoOperation::HashBlake3 { data, .. } => data.len(),
            CryptoOperation::DeriveKey { input, .. } => input.len(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum KeyDerivationAlgorithm {
    Blake3,
    HkdfSha256,
    HkdfSha512,
}

#[derive(Debug, Clone)]
pub struct CryptoResult {
    pub id: u64,
    pub result: Result<Vec<u8>, String>,
    pub processing_time: Duration,
    pub worker_id: usize,
    pub was_stolen: bool,
    pub queue_time: Duration,
}

#[derive(Debug, Clone)]
pub struct CryptoProcessorStats {
    pub total_tasks_submitted: u64,
    pub total_tasks_processed: u64,
    pub total_encryptions: u64,
    pub total_decryptions: u64,
    pub total_hashes: u64,
    pub total_derivations: u64,
    pub total_errors: u64,
    pub total_steals: u64,
    pub avg_processing_time_ns: f64,
    pub p95_processing_time_ns: f64,
    pub p99_processing_time_ns: f64,
    pub throughput: f64,
    pub queue_length: usize,
    pub worker_utilization: f64,
    pub simd_efficiency: f64,
    pub cache_hit_rate: f64,
}

pub struct OptimizedCryptoProcessor {
    worker_senders: Arc<Vec<Sender<CryptoTask>>>,
    worker_receivers: Arc<Vec<Receiver<CryptoTask>>>,
    _injector_sender: Sender<CryptoTask>,
    injector_receiver: Receiver<CryptoTask>,
    results: Arc<DashMap<u64, CryptoResult>>,
    pub performance_model: Arc<RwLock<CryptoPerformanceModel>>,
    work_stealing_model: Arc<RwLock<CryptoWorkStealingModel>>,
    batch_model: Arc<RwLock<BatchProcessingModel>>,
    stats: Arc<DashMap<String, u64>>,
    processing_times: Arc<Mutex<VecDeque<u64>>>,
    queue_lengths: Arc<Mutex<VecDeque<usize>>>,
    pub chacha20_accelerator: Arc<ChaCha20BatchAccelerator>,
    pub blake3_accelerator: Arc<Blake3BatchAccelerator>,
    is_running: Arc<std::sync::atomic::AtomicBool>,
    next_task_id: std::sync::atomic::AtomicU64,
    chacha_batch_buffer: Arc<Mutex<Vec<CryptoTask>>>,
    blake_batch_buffer: Arc<Mutex<Vec<CryptoTask>>>,
    derive_batch_buffer: Arc<Mutex<Vec<CryptoTask>>>,
    batch_timeout: Duration,
    max_batch_size: usize,
    enable_simd: bool,
    enable_work_stealing: bool,
}

impl OptimizedCryptoProcessor {
    pub fn new(num_workers: usize) -> Self {
        info!("🚀 Creating mathematical crypto processor v2.0");
        info!("  Workers: {}", num_workers);
        info!("  SIMD: ChaCha20 + Blake3");
        info!("  Batch processing: enabled");
        info!("  Work stealing: enabled");

        // Инициализация очередей
        let mut worker_senders = Vec::with_capacity(num_workers);
        let mut worker_receivers = Vec::with_capacity(num_workers);

        for _i in 0..num_workers {
            let (tx, rx) = bounded(1000);
            worker_senders.push(tx);
            worker_receivers.push(rx);
        }

        let (injector_sender, injector_receiver) = bounded(2000);

        // Инициализация моделей
        let performance_model = Arc::new(RwLock::new(CryptoPerformanceModel::new()));
        let work_stealing_model = Arc::new(RwLock::new(CryptoWorkStealingModel::new(num_workers)));
        let batch_model = Arc::new(RwLock::new(BatchProcessingModel::new(
            Duration::from_micros(500)
        )));

        // Инициализация SIMD акселераторов
        let chacha20_accelerator = Arc::new(ChaCha20BatchAccelerator::new(8));
        let blake3_accelerator = Arc::new(Blake3BatchAccelerator::new(8));

        let simd_info = chacha20_accelerator.get_simd_info();
        let blake3_info = blake3_accelerator.get_performance_info();

        info!("  SIMD Capabilities:");
        info!("    ChaCha20: AVX2={}, AVX512={}, NEON={}",
              simd_info.features.avx2, simd_info.features.avx512, simd_info.features.neon);
        info!("    Blake3: AVX2={}, AVX512={}, NEON={}",
              blake3_info.avx2_enabled, blake3_info.avx512_enabled, blake3_info.neon_enabled);
        info!("    Estimated throughput: {:.0} MB/s", blake3_info.estimated_throughput);

        let processor = Self {
            worker_senders: Arc::new(worker_senders),
            worker_receivers: Arc::new(worker_receivers),
            _injector_sender: injector_sender,
            injector_receiver,
            results: Arc::new(DashMap::with_capacity(10000)),
            performance_model,
            work_stealing_model,
            batch_model,
            stats: Arc::new(DashMap::new()),
            processing_times: Arc::new(Mutex::new(VecDeque::with_capacity(1000))),
            queue_lengths: Arc::new(Mutex::new(VecDeque::with_capacity(1000))),
            chacha20_accelerator,
            blake3_accelerator,
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            next_task_id: std::sync::atomic::AtomicU64::new(1),
            chacha_batch_buffer: Arc::new(Mutex::new(Vec::with_capacity(32))),
            blake_batch_buffer: Arc::new(Mutex::new(Vec::with_capacity(32))),
            derive_batch_buffer: Arc::new(Mutex::new(Vec::with_capacity(32))),
            batch_timeout: Duration::from_micros(100),
            max_batch_size: 32,
            enable_simd: true,
            enable_work_stealing: true,
        };

        processor.start_workers();
        processor.start_batch_processor();
        processor.start_stats_collector();

        info!("✅ Crypto processor initialized");

        processor
    }

    fn start_workers(&self) {
        let num_workers = self.worker_receivers.len();

        for worker_id in 0..num_workers {
            let worker_receiver = self.worker_receivers[worker_id].clone();
            let injector_receiver = self.injector_receiver.clone();
            let results = self.results.clone();
            let stats = self.stats.clone();
            let processing_times = self.processing_times.clone();
            let queue_lengths = self.queue_lengths.clone();
            let is_running = self.is_running.clone();

            let performance_model = self.performance_model.clone();
            let work_stealing_model = self.work_stealing_model.clone();

            let chacha20 = self.chacha20_accelerator.clone();
            let blake3 = self.blake3_accelerator.clone();

            let enable_simd = self.enable_simd;
            let enable_work_stealing = self.enable_work_stealing;

            tokio::spawn(async move {
                Self::worker_loop(
                    worker_id,
                    worker_receiver,
                    injector_receiver,
                    results,
                    stats,
                    processing_times,
                    queue_lengths,
                    is_running,
                    performance_model,
                    work_stealing_model,
                    chacha20,
                    blake3,
                    enable_simd,
                    enable_work_stealing,
                ).await;
            });
        }

        info!("✅ Started {} crypto workers", num_workers);
    }

    #[allow(clippy::too_many_arguments)]
    async fn worker_loop(
        worker_id: usize,
        worker_receiver: Receiver<CryptoTask>,
        injector_receiver: Receiver<CryptoTask>,
        results: Arc<DashMap<u64, CryptoResult>>,
        stats: Arc<DashMap<String, u64>>,
        processing_times: Arc<Mutex<VecDeque<u64>>>,
        queue_lengths: Arc<Mutex<VecDeque<usize>>>,
        is_running: Arc<std::sync::atomic::AtomicBool>,
        performance_model: Arc<RwLock<CryptoPerformanceModel>>,
        work_stealing_model: Arc<RwLock<CryptoWorkStealingModel>>,
        chacha20: Arc<ChaCha20BatchAccelerator>,
        blake3: Arc<Blake3BatchAccelerator>,
        enable_simd: bool,
        enable_work_stealing: bool,
    ) {
        debug!("🔐 Crypto worker #{} started", worker_id);

        let mut tasks_processed = 0;
        let mut total_processing_time = Duration::from_nanos(0);
        let mut last_steal_check = Instant::now();

        while is_running.load(std::sync::atomic::Ordering::Relaxed) {
            // Обновление длины очереди
            {
                let mut queue = queue_lengths.lock().await;
                queue.push_back(worker_receiver.len());
                if queue.len() > 1000 {
                    queue.pop_front();
                }
            }

            // Проверка необходимости кражи
            let should_steal = if enable_work_stealing && last_steal_check.elapsed() > Duration::from_millis(10) {
                last_steal_check = Instant::now();

                let model = work_stealing_model.read().await;
                let queue_len = worker_receiver.len();

                // Крадём, если очередь маленькая и есть дисбаланс
                queue_len < model.steal_threshold && model.imbalance > 0.3
            } else {
                false
            };

            tokio::select! {
                // Получение задачи из своей очереди
                Ok(task) = worker_receiver.recv_async() => {
                    let start_time = Instant::now();
                    let _queue_time = start_time.duration_since(task.created_at);

                    let _result = Self::process_task(
                        worker_id,
                        task,
                        &results,
                        &stats,
                        &performance_model,
                        &chacha20,
                        &blake3,
                        enable_simd,
                        false, // не украдено
                    ).await;

                    let processing_time = start_time.elapsed();
                    total_processing_time += processing_time;
                    tasks_processed += 1;

                    // Сохранение времени обработки
                    {
                        let mut times = processing_times.lock().await;
                        times.push_back(processing_time.as_nanos() as u64);
                        if times.len() > 1000 {
                            times.pop_front();
                        }
                    }

                    stats.entry(format!("worker_{}_tasks", worker_id))
                        .and_modify(|e| *e += 1)
                        .or_insert(1);

                    stats.entry(format!("worker_{}_time", worker_id))
                        .and_modify(|e| *e += processing_time.as_nanos() as u64)
                        .or_insert(processing_time.as_nanos() as u64);
                }

                // Кража задачи из injector
                Ok(task) = injector_receiver.recv_async(), if should_steal => {
                    stats.entry("crypto_steals".to_string())
                        .and_modify(|e| *e += 1)
                        .or_insert(1);

                    let start_time = Instant::now();
                    let _queue_time = start_time.duration_since(task.created_at);

                    let _result = Self::process_task(
                        worker_id,
                        task,
                        &results,
                        &stats,
                        &performance_model,
                        &chacha20,
                        &blake3,
                        enable_simd,
                        true, // украдено
                    ).await;

                    let _processing_time = start_time.elapsed();
                    tasks_processed += 1;
                }

                _ = tokio::time::sleep(Duration::from_micros(10)) => {
                    continue;
                }
            }
        }

        debug!("👋 Crypto worker #{} stopped, processed {} tasks",
               worker_id, tasks_processed);
    }

    async fn process_task(
        worker_id: usize,
        task: CryptoTask,
        results: &Arc<DashMap<u64, CryptoResult>>,
        stats: &Arc<DashMap<String, u64>>,
        _performance_model: &Arc<RwLock<CryptoPerformanceModel>>,
        chacha20: &Arc<ChaCha20BatchAccelerator>,
        blake3: &Arc<Blake3BatchAccelerator>,
        enable_simd: bool,
        was_stolen: bool,
    ) -> CryptoResult {
        let start_time = Instant::now();

        let result_data = match &task.operation {
            CryptoOperation::EncryptChaCha20 { key, nonce, plaintext } => {
                stats.entry("total_encryptions".to_string())
                    .and_modify(|e| *e += 1)
                    .or_insert(1);

                if enable_simd {
                    // SIMD ускорение
                    let keys = vec![*key];
                    let nonces = vec![*nonce];
                    let plaintexts = vec![plaintext.clone()];
                    let encrypted = chacha20.encrypt_batch(&keys, &nonces, &plaintexts).await;
                    encrypted[0].clone()
                } else {
                    // Скалярная реализация
                    use chacha20::cipher::{KeyIvInit, StreamCipher};
                    use chacha20::ChaCha20;

                    let mut cipher = ChaCha20::new(key.into(), nonce.into());
                    let mut buffer = plaintext.clone();
                    cipher.apply_keystream(&mut buffer);
                    buffer
                }
            }

            CryptoOperation::DecryptChaCha20 { key, nonce, ciphertext } => {
                stats.entry("total_decryptions".to_string())
                    .and_modify(|e| *e += 1)
                    .or_insert(1);

                if enable_simd {
                    let keys = vec![*key];
                    let nonces = vec![*nonce];
                    let ciphertexts = vec![ciphertext.clone()];
                    let decrypted = chacha20.decrypt_batch(&keys, &nonces, &ciphertexts).await;
                    decrypted[0].clone()
                } else {
                    use chacha20::cipher::{KeyIvInit, StreamCipher};
                    use chacha20::ChaCha20;

                    let mut cipher = ChaCha20::new(key.into(), nonce.into());
                    let mut buffer = ciphertext.clone();
                    cipher.apply_keystream(&mut buffer);
                    buffer
                }
            }

            CryptoOperation::HashBlake3 { key, data } => {
                stats.entry("total_hashes".to_string())
                    .and_modify(|e| *e += 1)
                    .or_insert(1);

                if enable_simd && data.len() >= 64 {
                    let keys = vec![*key];
                    let inputs = vec![data.clone()];
                    let hashes = blake3.hash_keyed_batch(&keys, &inputs).await;
                    hashes[0].to_vec()
                } else {
                    use blake3::Hasher;
                    let mut hasher = Hasher::new_keyed(key);
                    hasher.update(data);
                    let hash = hasher.finalize();
                    hash.as_bytes().to_vec()
                }
            }

            CryptoOperation::DeriveKey { algorithm, input, context, output_len } => {
                stats.entry("total_derivations".to_string())
                    .and_modify(|e| *e += 1)
                    .or_insert(1);

                match algorithm {
                    KeyDerivationAlgorithm::Blake3 => {
                        use blake3::Hasher;
                        let mut hasher = Hasher::new();
                        hasher.update(input);
                        hasher.update(context);
                        let mut output = vec![0u8; *output_len];
                        hasher.finalize_xof().fill(&mut output);
                        output
                    }
                    KeyDerivationAlgorithm::HkdfSha256 => {
                        use ring::hkdf;
                        let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]);
                        let prk = salt.extract(input);
                        let context_slice = &[context.as_slice()];
                        let mut output = vec![0u8; *output_len];
                        if let Ok(okm) = prk.expand(context_slice, hkdf::HKDF_SHA256) {
                            let _ = okm.fill(&mut output);
                        }
                        output
                    }
                    KeyDerivationAlgorithm::HkdfSha512 => {
                        use ring::hkdf;
                        let salt = hkdf::Salt::new(hkdf::HKDF_SHA512, &[]);
                        let prk = salt.extract(input);
                        let context_slice = &[context.as_slice()];
                        let mut output = vec![0u8; *output_len];
                        if let Ok(okm) = prk.expand(context_slice, hkdf::HKDF_SHA512) {
                            let _ = okm.fill(&mut output);
                        }
                        output
                    }
                }
            }
        };

        let processing_time = start_time.elapsed();

        let result = CryptoResult {
            id: task.id,
            result: Ok(result_data),
            processing_time,
            worker_id,
            was_stolen,
            queue_time: start_time.duration_since(task.created_at),
        };

        results.insert(task.id, result.clone());

        stats.entry("total_tasks_processed".to_string())
            .and_modify(|e| *e += 1)
            .or_insert(1);

        result
    }

    fn start_batch_processor(&self) {
        let chacha_buffer = self.chacha_batch_buffer.clone();
        let blake_buffer = self.blake_batch_buffer.clone();
        let derive_buffer = self.derive_batch_buffer.clone();
        let chacha20 = self.chacha20_accelerator.clone();
        let blake3 = self.blake3_accelerator.clone();
        let results = self.results.clone();
        let stats = self.stats.clone();
        let is_running = self.is_running.clone();
        let batch_timeout = self.batch_timeout;
        let _max_batch_size = self.max_batch_size;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(batch_timeout);

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                // Обработка ChaCha20 батча
                let chacha_tasks = {
                    let mut buffer = chacha_buffer.lock().await;
                    if buffer.len() >= 4 {
                        let batch = buffer.clone();
                        buffer.clear();
                        batch
                    } else {
                        Vec::new()
                    }
                };

                if !chacha_tasks.is_empty() {
                    Self::process_chacha_batch(
                        chacha_tasks,
                        &results,
                        &stats,
                        &chacha20,
                    ).await;
                }

                // Обработка Blake3 батча
                let blake_tasks = {
                    let mut buffer = blake_buffer.lock().await;
                    if buffer.len() >= 4 {
                        let batch = buffer.clone();
                        buffer.clear();
                        batch
                    } else {
                        Vec::new()
                    }
                };

                if !blake_tasks.is_empty() {
                    Self::process_blake_batch(
                        blake_tasks,
                        &results,
                        &stats,
                        &blake3,
                    ).await;
                }

                // Обработка Derive батча
                let derive_tasks = {
                    let mut buffer = derive_buffer.lock().await;
                    if buffer.len() >= 2 {
                        let batch = buffer.clone();
                        buffer.clear();
                        batch
                    } else {
                        Vec::new()
                    }
                };

                for _task in derive_tasks {
                    // TODO
                    // Derive операции обрабатываются по одной
                    // (не поддерживают SIMD)
                }
            }
        });
    }

    async fn process_chacha_batch(
        tasks: Vec<CryptoTask>,
        results: &Arc<DashMap<u64, CryptoResult>>,
        stats: &Arc<DashMap<String, u64>>,
        chacha20: &Arc<ChaCha20BatchAccelerator>,
    ) {
        let start_time = Instant::now();
        let batch_size = tasks.len();

        let mut keys = Vec::with_capacity(batch_size);
        let mut nonces = Vec::with_capacity(batch_size);
        let mut data_buffers = Vec::with_capacity(batch_size);
        let mut is_encryption = Vec::with_capacity(batch_size);
        let mut task_ids = Vec::with_capacity(batch_size);

        for task in &tasks {
            match &task.operation {
                CryptoOperation::EncryptChaCha20 { key, nonce, plaintext } => {
                    keys.push(*key);
                    nonces.push(*nonce);
                    data_buffers.push(plaintext.clone());
                    is_encryption.push(true);
                    task_ids.push(task.id);
                }
                CryptoOperation::DecryptChaCha20 { key, nonce, ciphertext } => {
                    keys.push(*key);
                    nonces.push(*nonce);
                    data_buffers.push(ciphertext.clone());
                    is_encryption.push(false);
                    task_ids.push(task.id);
                }
                _ => {}
            }
        }

        let processed = if is_encryption.iter().all(|&x| x) {
            chacha20.encrypt_batch(&keys, &nonces, &data_buffers).await
        } else {
            chacha20.decrypt_batch(&keys, &nonces, &data_buffers).await
        };

        for (i, task_id) in task_ids.iter().enumerate() {
            if i < processed.len() {
                let result = CryptoResult {
                    id: *task_id,
                    result: Ok(processed[i].clone()),
                    processing_time: start_time.elapsed(),
                    worker_id: 999, // batch processor
                    was_stolen: false,
                    queue_time: Duration::from_nanos(0),
                };
                results.insert(*task_id, result);
            }
        }

        stats.entry("chacha_batch_operations".to_string())
            .and_modify(|e| *e += batch_size as u64)
            .or_insert(batch_size as u64);

        stats.entry("chacha_batches".to_string())
            .and_modify(|e| *e += 1)
            .or_insert(1);
    }

    async fn process_blake_batch(
        tasks: Vec<CryptoTask>,
        results: &Arc<DashMap<u64, CryptoResult>>,
        stats: &Arc<DashMap<String, u64>>,
        blake3: &Arc<Blake3BatchAccelerator>,
    ) {
        let start_time = Instant::now();
        let batch_size = tasks.len();

        let mut keys = Vec::with_capacity(batch_size);
        let mut inputs = Vec::with_capacity(batch_size);
        let mut task_ids = Vec::with_capacity(batch_size);

        for task in &tasks {
            if let CryptoOperation::HashBlake3 { key, data } = &task.operation {
                keys.push(*key);
                inputs.push(data.clone());
                task_ids.push(task.id);
            }
        }

        let hashes = blake3.hash_keyed_batch(&keys, &inputs).await;

        for (i, task_id) in task_ids.iter().enumerate() {
            if i < hashes.len() {
                let result = CryptoResult {
                    id: *task_id,
                    result: Ok(hashes[i].to_vec()),
                    processing_time: start_time.elapsed(),
                    worker_id: 999, // batch processor
                    was_stolen: false,
                    queue_time: Duration::from_nanos(0),
                };
                results.insert(*task_id, result);
            }
        }

        stats.entry("blake_batch_operations".to_string())
            .and_modify(|e| *e += batch_size as u64)
            .or_insert(batch_size as u64);

        stats.entry("blake_batches".to_string())
            .and_modify(|e| *e += 1)
            .or_insert(1);
    }

    fn start_stats_collector(&self) {
        let stats = self.stats.clone();
        let processing_times = self.processing_times.clone();
        let queue_lengths = self.queue_lengths.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                // Расчёт перцентилей времени обработки
                let times = processing_times.lock().await;
                if !times.is_empty() {
                    let mut times_vec: Vec<u64> = times.iter().copied().collect();
                    times_vec.sort_unstable();

                    let len = times_vec.len();
                    let p95 = times_vec[len * 95 / 100];
                    let p99 = times_vec[len * 99 / 100];

                    stats.insert("p95_processing_time_ns".to_string(), p95);
                    stats.insert("p99_processing_time_ns".to_string(), p99);
                }
                drop(times);

                // Средняя длина очереди
                let queues = queue_lengths.lock().await;
                if !queues.is_empty() {
                    let avg_queue = queues.iter().sum::<usize>() as f64 / queues.len() as f64;
                    stats.insert("avg_queue_length".to_string(), (avg_queue * 1000.0) as u64);
                }
                drop(queues);

                // Пропускная способность
                let processed = stats.get("total_tasks_processed").map(|v| *v.value()).unwrap_or(0);
                let elapsed = 5; // секунд
                let throughput = processed as f64 / elapsed as f64;
                stats.insert("throughput".to_string(), (throughput * 1000.0) as u64);
            }
        });
    }
    
    pub fn get_stats(&self) -> HashMap<String, u64> {
        let mut stats_map = HashMap::new();

        for entry in self.stats.iter() {
            stats_map.insert(entry.key().clone(), *entry.value());
        }

        // Добавляем рассчитанные метрики
        if let Ok(times) = self.processing_times.try_lock() {
            if !times.is_empty() {
                let avg_time = times.iter().sum::<u64>() / times.len() as u64;
                stats_map.insert("avg_processing_time_ns".to_string(), avg_time);
            }
        }

        stats_map
    }

    pub async fn shutdown(&self) {
        info!("🛑 Shutting down crypto processor...");
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);

        // Очистка буферов
        self.chacha_batch_buffer.lock().await.clear();
        self.blake_batch_buffer.lock().await.clear();
        self.derive_batch_buffer.lock().await.clear();

        info!("✅ Crypto processor stopped");
    }
}

impl Drop for OptimizedCryptoProcessor {
    fn drop(&mut self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Clone for OptimizedCryptoProcessor {
    fn clone(&self) -> Self {
        let num_workers = self.worker_senders.len();
        let mut worker_senders = Vec::with_capacity(num_workers);
        let mut worker_receivers = Vec::with_capacity(num_workers);

        for _i in 0..num_workers {
            let (tx, rx) = bounded(1000);
            worker_senders.push(tx);
            worker_receivers.push(rx);
        }

        let (injector_sender, injector_receiver) = bounded(2000);

        Self {
            worker_senders: Arc::new(worker_senders),
            worker_receivers: Arc::new(worker_receivers),
            _injector_sender: injector_sender,
            injector_receiver,
            results: self.results.clone(),
            performance_model: self.performance_model.clone(),
            work_stealing_model: self.work_stealing_model.clone(),
            batch_model: self.batch_model.clone(),
            stats: self.stats.clone(),
            processing_times: Arc::new(Mutex::new(VecDeque::new())),
            queue_lengths: Arc::new(Mutex::new(VecDeque::new())),
            chacha20_accelerator: self.chacha20_accelerator.clone(),
            blake3_accelerator: self.blake3_accelerator.clone(),
            is_running: self.is_running.clone(),
            next_task_id: std::sync::atomic::AtomicU64::new(
                self.next_task_id.load(std::sync::atomic::Ordering::Relaxed)
            ),
            chacha_batch_buffer: Arc::new(Mutex::new(Vec::new())),
            blake_batch_buffer: Arc::new(Mutex::new(Vec::new())),
            derive_batch_buffer: Arc::new(Mutex::new(Vec::new())),
            batch_timeout: self.batch_timeout,
            max_batch_size: self.max_batch_size,
            enable_simd: self.enable_simd,
            enable_work_stealing: self.enable_work_stealing,
        }
    }
}