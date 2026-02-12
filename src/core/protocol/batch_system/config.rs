use std::time::{Duration, Instant};
use std::collections::HashMap;

/// Модель оптимизации конфигурации на основе теории массового обслуживания
#[derive(Debug, Clone)]
pub struct ConfigOptimizationModel {
    /// Интенсивность входящего потока (λ)
    pub arrival_rate: f64,

    /// Интенсивность обработки (μ)
    pub service_rate: f64,

    /// Коэффициент загрузки (ρ)
    pub utilization: f64,

    /// Целевая задержка (L_target)
    pub target_latency: Duration,

    /// Максимальная пропускная способность
    pub max_throughput: f64,

    /// Количество ядер CPU
    pub cpu_cores: usize,

    /// Доступная память (байт)
    pub available_memory: usize,

    /// Сетевая пропускная способность (бит/с)
    pub network_bandwidth: f64,
}

impl ConfigOptimizationModel {
    pub fn new() -> Self {
        Self {
            arrival_rate: 1000.0,
            service_rate: 10000.0,
            utilization: 0.7,
            target_latency: Duration::from_millis(50),
            max_throughput: 100000.0,
            cpu_cores: num_cpus::get(),
            available_memory: 8 * 1024 * 1024 * 1024, // 8 GB
            network_bandwidth: 1_000_000_000.0, // 1 Gbps
        }
    }

    /// Расчёт оптимального количества воркеров: M = ⌈λ/(μ·ρ)⌉
    pub fn optimal_worker_count(&self) -> usize {
        if self.service_rate <= 0.0 {
            return self.cpu_cores;
        }

        let workers = (self.arrival_rate / (self.service_rate * self.utilization)).ceil() as usize;
        workers.clamp(1, self.cpu_cores * 4)
    }

    /// Расчёт оптимального размера очереди: K = ⌈ln(ε)/ln(ρ)⌉
    pub fn optimal_queue_size(&self, loss_probability: f64) -> usize {
        if self.utilization >= 1.0 {
            return 100000;
        }

        let k = (loss_probability.ln() / self.utilization.ln()).ceil() as usize;
        k.clamp(1000, 100000)
    }

    /// Расчёт оптимального размера батча: B* = √(α·λ/γ)
    pub fn optimal_batch_size(&self, alpha: f64, gamma: f64) -> usize {
        if gamma <= 0.0 || self.arrival_rate <= 0.0 {
            return 256;  // БЫЛО: 256, СТАЛО: 256 (константа)
        }

        // ИСПРАВЛЕНО: более консервативная формула
        let b = ((alpha * 1000.0) / gamma).sqrt().round() as usize;  // убрали λ из формулы
        b.clamp(64, 512)  // сузили диапазон
    }

    /// Расчёт оптимального таймаута чтения: T_read = B/λ + 3σ
    pub fn optimal_read_timeout(&self, batch_size: usize, std_dev: f64) -> Duration {
        let base_time = batch_size as f64 / self.arrival_rate.max(1.0);
        let timeout = base_time + 3.0 * std_dev;
        Duration::from_secs_f64(timeout.clamp(1.0, 30.0))
    }

    /// Расчёт оптимального интервала сброса: T_flush = L_target / 2
    pub fn optimal_flush_interval(&self) -> Duration {
        Duration::from_millis(self.target_latency.as_millis() as u64 / 2)
    }

    /// Расчёт оптимального размера буфера: buf_size = B·M·4
    pub fn optimal_buffer_size(&self, batch_size: usize, workers: usize) -> usize {
        (batch_size * workers * 4).clamp(1024, 262144)
    }
}

#[derive(Debug, Clone)]
pub struct AdaptiveConfigState {
    /// История изменений конфигурации
    pub history: Vec<ConfigChangeRecord>,

    /// Текущая версия конфигурации
    pub version: u64,

    /// Время последнего изменения
    pub last_change: Instant,

    /// Причина последнего изменения
    pub last_change_reason: String,

    /// Метрики эффективности конфигурации
    pub effectiveness: f64,
}

#[derive(Debug, Clone)]
pub struct ConfigChangeRecord {
    pub timestamp: Instant,
    pub version: u64,
    pub parameter: String,
    pub old_value: String,
    pub new_value: String,
    pub reason: String,
    pub effectiveness_delta: f64,
}

impl AdaptiveConfigState {
    pub fn new() -> Self {
        Self {
            history: Vec::with_capacity(100),
            version: 1,
            last_change: Instant::now(),
            last_change_reason: "Initial configuration".to_string(),
            effectiveness: 1.0,
        }
    }

    pub fn record_change(&mut self, parameter: String, old: String, new: String, reason: String) {
        self.version += 1;

        let record = ConfigChangeRecord {
            timestamp: Instant::now(),
            version: self.version,
            parameter,
            old_value: old,
            new_value: new,
            reason: reason.clone(),
            effectiveness_delta: 0.0,
        };

        self.history.push(record);
        self.last_change = Instant::now();
        self.last_change_reason = reason;

        if self.history.len() > 100 {
            self.history.remove(0);
        }
    }
}

/// Динамическая математическая конфигурация batch системы
#[derive(Debug, Clone)]
pub struct BatchConfig {
    pub read_buffer_size: usize,
    pub read_timeout: Duration,
    pub max_concurrent_reads: usize,
    pub adaptive_read_timeout: bool,
    pub write_buffer_size: usize,
    pub write_timeout: Duration,
    pub max_pending_writes: usize,
    pub flush_interval: Duration,
    pub enable_qos: bool,
    pub batch_size: usize,
    pub min_batch_size: usize,
    pub max_batch_size: usize,
    pub enable_adaptive_batching: bool,
    pub adaptive_batch_window: Duration,
    pub batch_accumulation_timeout: Duration,
    pub worker_count: usize,
    pub max_queue_size: usize,
    pub enable_work_stealing: bool,
    pub load_balancing_interval: Duration,
    pub enable_load_aware_scheduling: bool,
    pub steal_threshold: usize,
    pub steal_probability: f64,
    pub buffer_preallocation_size: usize,
    pub max_concurrent_batches: usize,
    pub enable_monitoring: bool,
    pub shrink_interval: Duration,
    pub buffer_cleanup_threshold: f64,
    pub enable_buffer_compression: bool,
    pub circuit_breaker_enabled: bool,
    pub failure_threshold: usize,
    pub recovery_timeout: Duration,
    pub half_open_max_requests: usize,
    pub consecutive_successes_needed: usize,
    pub backoff_factor: f64,
    pub qos_enabled: bool,
    pub high_priority_quota: f64,
    pub normal_priority_quota: f64,
    pub low_priority_quota: f64,
    pub enable_dynamic_qos: bool,
    pub qos_adaptation_interval: Duration,
    pub metrics_enabled: bool,
    pub metrics_collection_interval: Duration,
    pub trace_sampling_rate: f64,
    pub metrics_retention_period: Duration,
    pub enable_detailed_stats: bool,
    pub enable_simd_acceleration: bool,
    pub simd_batch_size: usize,
    pub simd_alignment: usize,
    pub enable_avx2: bool,
    pub enable_avx512: bool,
    pub enable_neon: bool,
    pub crypto_worker_count: usize,
    pub crypto_batch_size: usize,
    pub crypto_queue_size: usize,
    pub enable_crypto_stealing: bool,
    pub optimization_model: ConfigOptimizationModel,
    pub adaptive_state: AdaptiveConfigState,
    pub user_params: HashMap<String, String>,
}

impl Default for BatchConfig {
    fn default() -> Self {
        let cpu_count = num_cpus::get();
        let optimal_worker_count = cpu_count * 2;

        let optimization_model = ConfigOptimizationModel::new();
        let adaptive_state = AdaptiveConfigState::new();

        info!("⚙️ Initializing Mathematical BatchConfig v2.0");
        info!("  CPU cores: {}", cpu_count);
        info!("  Optimal workers: {}", optimal_worker_count);
        info!("  Target latency: {:?}", optimization_model.target_latency);
        info!("  Max throughput: {:.0} ops/s", optimization_model.max_throughput);

        Self {
            read_buffer_size: 32768,
            read_timeout: Duration::from_secs(10),
            max_concurrent_reads: 10000,
            adaptive_read_timeout: true,
            write_buffer_size: 65536,
            write_timeout: Duration::from_secs(5),
            max_pending_writes: 50000,
            flush_interval: Duration::from_millis(10),
            enable_qos: true,
            batch_size: optimization_model.optimal_batch_size(0.5, 0.001),
            min_batch_size: 32,
            max_batch_size: 1024,
            enable_adaptive_batching: true,
            adaptive_batch_window: Duration::from_secs(1),
            batch_accumulation_timeout: Duration::from_millis(50),
            worker_count: optimal_worker_count,
            max_queue_size: optimization_model.optimal_queue_size(0.001),
            enable_work_stealing: true,
            load_balancing_interval: Duration::from_millis(250),
            enable_load_aware_scheduling: true,
            steal_threshold: 10,
            steal_probability: 0.1,
            buffer_preallocation_size: optimization_model.optimal_buffer_size(256, optimal_worker_count),
            max_concurrent_batches: cpu_count * 4,
            enable_monitoring: true,
            shrink_interval: Duration::from_secs(60),
            buffer_cleanup_threshold: 0.3,
            enable_buffer_compression: false,
            circuit_breaker_enabled: true,
            failure_threshold: 50,
            recovery_timeout: Duration::from_secs(30),
            half_open_max_requests: 10,
            consecutive_successes_needed: 5,
            backoff_factor: 2.0,
            qos_enabled: true,
            high_priority_quota: 0.4,
            normal_priority_quota: 0.4,
            low_priority_quota: 0.2,
            enable_dynamic_qos: true,
            qos_adaptation_interval: Duration::from_secs(30),
            metrics_enabled: true,
            metrics_collection_interval: Duration::from_secs(5),
            trace_sampling_rate: 0.1,
            metrics_retention_period: Duration::from_secs(3600),
            enable_detailed_stats: true,
            enable_simd_acceleration: true,
            simd_batch_size: 8,
            simd_alignment: 64,
            enable_avx2: is_x86_feature_detected!("avx2"),
            enable_avx512: is_x86_feature_detected!("avx512f"),
            enable_neon: cfg!(target_arch = "aarch64"),
            crypto_worker_count: cpu_count * 2,
            crypto_batch_size: 32,
            crypto_queue_size: 2000,
            enable_crypto_stealing: true,
            optimization_model,
            adaptive_state,
            user_params: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigStats {
    pub version: u64,
    pub last_change: Instant,
    pub last_change_reason: String,
    pub worker_count: usize,
    pub batch_size: usize,
    pub queue_size: usize,
    pub read_timeout_ms: u64,
    pub write_timeout_ms: u64,
    pub flush_interval_ms: u64,
    pub buffer_size_kb: usize,
    pub simd_enabled: bool,
    pub qos_quotas: (f64, f64, f64),
    pub circuit_breaker_threshold: usize,
    pub recovery_timeout_secs: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Invalid configuration value: {0}")]
    InvalidValue(String),

    #[error("Missing required parameter: {0}")]
    MissingParameter(String),

    #[error("Configuration conflict: {0}")]
    Conflict(String),
}

#[cfg(target_arch = "x86_64")]
use std::arch::is_x86_feature_detected;

#[cfg(target_arch = "aarch64")]
use std::arch::is_aarch64_feature_detected;
use tracing::info;