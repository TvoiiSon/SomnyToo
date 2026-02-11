use std::time::Duration;

/// Динамическая конфигурация batch системы - ОПТИМИЗИРОВАННАЯ ВЕРСИЯ
#[derive(Debug, Clone)]
pub struct BatchConfig {
    // Чтение
    pub read_buffer_size: usize,
    pub read_timeout: Duration,
    pub max_concurrent_reads: usize,
    pub adaptive_read_timeout: bool,

    // Запись
    pub write_buffer_size: usize,
    pub write_timeout: Duration,
    pub max_pending_writes: usize,
    pub flush_interval: Duration,
    pub enable_qos: bool,

    // Обработка
    pub batch_size: usize,
    pub min_batch_size: usize,
    pub max_batch_size: usize,
    pub enable_adaptive_batching: bool,
    pub adaptive_batch_window: Duration,

    // Диспетчер
    pub worker_count: usize,
    pub max_queue_size: usize,
    pub enable_work_stealing: bool,
    pub load_balancing_interval: Duration,
    pub enable_load_aware_scheduling: bool,

    // Буферы
    pub buffer_preallocation_size: usize,
    pub max_concurrent_batches: usize,
    pub enable_monitoring: bool,
    pub shrink_interval: Duration,

    // Circuit Breaker
    pub circuit_breaker_enabled: bool,
    pub failure_threshold: usize,
    pub recovery_timeout: Duration,
    pub half_open_max_requests: usize,

    // QoS
    pub qos_enabled: bool,
    pub high_priority_quota: f64,
    pub normal_priority_quota: f64,
    pub low_priority_quota: f64,

    // Metrics
    pub metrics_enabled: bool,
    pub metrics_collection_interval: Duration,
    pub trace_sampling_rate: f64,

    // SIMD Акселерация
    pub enable_simd_acceleration: bool,
    pub simd_batch_size: usize,
}

impl Default for BatchConfig {
    fn default() -> Self {
        let cpu_count = num_cpus::get();
        let optimal_worker_count = cpu_count * 2;

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

            batch_size: 256,
            min_batch_size: 64,
            max_batch_size: 2048,
            enable_adaptive_batching: true,
            adaptive_batch_window: Duration::from_secs(1),

            worker_count: optimal_worker_count,
            max_queue_size: 100000,
            enable_work_stealing: true,
            load_balancing_interval: Duration::from_millis(250),
            enable_load_aware_scheduling: true,

            buffer_preallocation_size: 65536,
            max_concurrent_batches: cpu_count * 4,
            enable_monitoring: true,
            shrink_interval: Duration::from_secs(60),

            circuit_breaker_enabled: true,
            failure_threshold: 50,
            recovery_timeout: Duration::from_secs(30),
            half_open_max_requests: 10,

            qos_enabled: true,
            high_priority_quota: 0.4,
            normal_priority_quota: 0.4,
            low_priority_quota: 0.2,

            metrics_enabled: true,
            metrics_collection_interval: Duration::from_secs(5),
            trace_sampling_rate: 0.1,

            enable_simd_acceleration: true,
            simd_batch_size: 8,
        }
    }
}