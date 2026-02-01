use std::time::Duration;

/// Конфигурация пакетного чтения
#[derive(Debug, Clone)]
pub struct BatchReaderConfig {
    pub batch_size: usize,           // Оптимальный размер батча
    pub buffer_size: usize,          // Размер буфера чтения
    pub read_timeout: Duration,      // Таймаут на чтение
    pub max_pending_batches: usize,  // Максимальное количество ожидающих батчей
    pub enable_adaptive_batching: bool, // Адаптивный размер батча
    pub min_batch_size: usize,       // Минимальный размер батча
    pub max_batch_size: usize,       // Максимальный размер батча
}

impl Default for BatchReaderConfig {
    fn default() -> Self {
        Self {
            batch_size: 64,
            buffer_size: 65536,      // 64KB
            read_timeout: Duration::from_secs(30000),
            max_pending_batches: 100,
            enable_adaptive_batching: true,
            min_batch_size: 8,
            max_batch_size: 256,
        }
    }
}