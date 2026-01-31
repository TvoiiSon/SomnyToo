/// Статистика читателя
#[derive(Debug, Clone, Default)]
pub struct ReaderStats {
    pub total_frames_read: u64,
    pub total_bytes_read: u64,
    pub total_batches_processed: u64,
    pub avg_batch_size: f64,
    pub avg_frame_size: f64,
    pub read_timeouts: u64,
    pub read_errors: u64,
    pub current_pending_batches: usize,
    pub frames_per_second: f64,
    pub bytes_per_second: f64,
}