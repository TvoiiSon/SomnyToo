use std::time::Duration;

/// Статистика писателя
#[derive(Debug, Clone, Default)]
pub struct WriterStats {
    pub total_writes: u64,
    pub total_bytes_written: u64,
    pub total_batches_written: u64,
    pub avg_batch_size: f64,
    pub avg_write_time: Duration,
    pub write_timeouts: u64,
    pub write_errors: u64,
    pub buffer_hits: u64,           // Буферизованные записи
    pub immediate_writes: u64,      // Немедленные записи
    pub current_buffer_size: usize,
    pub writes_per_second: f64,
    pub bytes_per_second: f64,
}