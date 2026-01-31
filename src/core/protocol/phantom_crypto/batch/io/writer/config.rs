use std::time::Duration;

/// Конфигурация пакетной записи
#[derive(Debug, Clone)]
pub struct BatchWriterConfig {
    pub batch_size: usize,           // Размер батча для записи
    pub buffer_size: usize,          // Размер буфера записи
    pub write_timeout: Duration,     // Таймаут на запись
    pub flush_interval: Duration,    // Интервал автоматического сброса
    pub max_pending_writes: usize,   // Максимальное количество ожидающих записей
    pub enable_buffering: bool,      // Включить буферизацию
    pub max_buffer_size: usize,      // Максимальный размер буфера
    pub enable_nagle: bool,          // Алгоритм Nagle для TCP
}

impl Default for BatchWriterConfig {
    fn default() -> Self {
        Self {
            batch_size: 64,
            buffer_size: 65536,      // 64KB
            write_timeout: Duration::from_secs(5),
            flush_interval: Duration::from_millis(100),
            max_pending_writes: 1000,
            enable_buffering: true,
            max_buffer_size: 1024 * 1024, // 1MB
            enable_nagle: false,
        }
    }
}