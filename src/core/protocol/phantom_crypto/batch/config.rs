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
        let cpu_count = 16; // AMD Ryzen 7 7735HS имеет 8 ядер/16 потоков
        
        Self {
            // Чтение - оптимизировано для 10K+ соединений
            read_buffer_size: 32768,        // 32KB для снижения syscall
            read_timeout: Duration::from_secs(60),  // Долгие keep-alive соединения
            max_concurrent_reads: 10000,    // Поддержка 10K параллельных подключений

            // Запись - максимальная производительность
            write_buffer_size: 65536,       // 64KB буфер записи
            write_timeout: Duration::from_secs(30),
            max_pending_writes: 100000,     // 100K сообщений в очереди
            flush_interval: Duration::from_millis(1), // Агрессивный flush для low-latency

            // Обработка - экстремальный батчинг
            batch_size: 256,                // Большие батчи для throughput
            min_batch_size: 64,             // Быстрый отклик
            max_batch_size: 4096,           // Огромные батчи для пиковой нагрузки
            enable_adaptive_batching: true, // Критически важно!

            // Диспетчер - максимальный параллелизм
            worker_count: cpu_count * 8,    // 128 воркеров! (16*8)
            max_queue_size: 500000,         // Полмиллиона задач в очереди
            enable_work_stealing: true,
            load_balancing_interval: Duration::from_millis(100), // Супер-частая балансировка

            // Буферы - преаллокация для нулевых аллокаций
            buffer_preallocation_size: 262144,  // 256KB преаллокация
            max_concurrent_batches: cpu_count * 16, // 256 параллельных батчей
            enable_monitoring: true,
            shrink_interval: Duration::from_secs(120), // Редкое сжатие
        }
    }
}