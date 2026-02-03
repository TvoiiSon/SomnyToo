use std::time::Duration;

/// Конфигурация всей batch системы
#[derive(Debug, Clone)]
pub struct BatchConfig {
    // Чтение
    pub read_buffer_size: usize,
    pub read_timeout: Duration,
    pub max_concurrent_reads: usize,

    // Запись
    pub write_buffer_size: usize,
    pub write_timeout: Duration,
    pub max_pending_writes: usize,
    pub flush_interval: Duration,

    // Обработка
    pub batch_size: usize,
    pub min_batch_size: usize,
    pub max_batch_size: usize,
    pub enable_adaptive_batching: bool,

    // Диспетчер
    pub worker_count: usize,
    pub max_queue_size: usize,
    pub enable_work_stealing: bool,
    pub load_balancing_interval: Duration,

    // Буферы
    pub buffer_preallocation_size: usize,
    pub max_concurrent_batches: usize,
    pub enable_monitoring: bool,
    pub shrink_interval: Duration,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            read_buffer_size: 8192,
            read_timeout: Duration::from_secs(10),
            max_concurrent_reads: 100,

            write_buffer_size: 8192,
            write_timeout: Duration::from_secs(10),
            max_pending_writes: 1000,
            flush_interval: Duration::from_millis(10),

            batch_size: 64,
            min_batch_size: 8,
            max_batch_size: 256,
            enable_adaptive_batching: true,

            worker_count: 4,
            max_queue_size: 10000,
            enable_work_stealing: true,
            load_balancing_interval: Duration::from_secs(1),

            buffer_preallocation_size: 65536,
            max_concurrent_batches: 32,
            enable_monitoring: true,
            shrink_interval: Duration::from_secs(30),
        }
    }
}