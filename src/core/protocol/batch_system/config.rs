use std::time::Duration;

/// Динамическая конфигурация batch системы
#[derive(Debug, Clone)]
pub struct BatchConfig {
    // Чтение
    pub read_buffer_size: usize,
    pub read_timeout: Duration,
    pub max_concurrent_reads: usize,
    pub adaptive_read_timeout: bool, // NEW

    // Запись
    pub write_buffer_size: usize,
    pub write_timeout: Duration,
    pub max_pending_writes: usize,
    pub flush_interval: Duration,
    pub enable_qos: bool, // NEW

    // Обработка
    pub batch_size: usize,
    pub min_batch_size: usize,
    pub max_batch_size: usize,
    pub enable_adaptive_batching: bool,
    pub adaptive_batch_window: Duration, // NEW

    // Диспетчер
    pub worker_count: usize,
    pub max_queue_size: usize,
    pub enable_work_stealing: bool,
    pub load_balancing_interval: Duration,
    pub enable_load_aware_scheduling: bool, // NEW

    // Буферы
    pub buffer_preallocation_size: usize,
    pub max_concurrent_batches: usize,
    pub enable_monitoring: bool,
    pub shrink_interval: Duration,

    // Circuit Breaker
    pub circuit_breaker_enabled: bool, // NEW
    pub failure_threshold: usize,
    pub recovery_timeout: Duration,
    pub half_open_max_requests: usize,

    // QoS
    pub qos_enabled: bool, // NEW
    pub high_priority_quota: f64, // 0.3 = 30% ресурсов
    pub normal_priority_quota: f64,
    pub low_priority_quota: f64,

    // Metrics
    pub metrics_enabled: bool, // NEW
    pub metrics_collection_interval: Duration,
    pub trace_sampling_rate: f64,
}

impl Default for BatchConfig {
    fn default() -> Self {
        let cpu_count = num_cpus::get();

        // Оптимальное количество воркеров: 2-4x физических ядер
        let optimal_worker_count = cpu_count * 2; // 32 для 16 ядер

        Self {
            // Чтение
            read_buffer_size: 32768,
            read_timeout: Duration::from_secs(10), // Уменьшено с 60
            max_concurrent_reads: 10000,
            adaptive_read_timeout: true,

            // Запись
            write_buffer_size: 65536,
            write_timeout: Duration::from_secs(5), // Уменьшено с 30
            max_pending_writes: 50000, // Уменьшено с 100000
            flush_interval: Duration::from_millis(10), // Увеличено с 1
            enable_qos: true,

            // Обработка
            batch_size: 128, // Уменьшено с 256
            min_batch_size: 32, // Уменьшено с 64
            max_batch_size: 1024, // Уменьшено с 4096
            enable_adaptive_batching: true,
            adaptive_batch_window: Duration::from_secs(1),

            // Диспетчер
            worker_count: optimal_worker_count, // 32 вместо 128
            max_queue_size: 100000, // Уменьшено с 500000
            enable_work_stealing: true,
            load_balancing_interval: Duration::from_millis(250), // Увеличено с 100
            enable_load_aware_scheduling: true,

            // Буферы
            buffer_preallocation_size: 65536, // Уменьшено с 262144
            max_concurrent_batches: cpu_count * 4, // Уменьшено с 16
            enable_monitoring: true,
            shrink_interval: Duration::from_secs(60), // Уменьшено с 120

            // Circuit Breaker
            circuit_breaker_enabled: true,
            failure_threshold: 50,
            recovery_timeout: Duration::from_secs(30),
            half_open_max_requests: 10,

            // QoS
            qos_enabled: true,
            high_priority_quota: 0.4, // 40% для критического трафика
            normal_priority_quota: 0.4, // 40% для нормального
            low_priority_quota: 0.2, // 20% для фонового

            // Metrics
            metrics_enabled: true,
            metrics_collection_interval: Duration::from_secs(5),
            trace_sampling_rate: 0.1, // 10% трассировки
        }
    }
}