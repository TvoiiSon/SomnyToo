#[derive(Debug, Clone)]
pub struct BatchStats {
    pub total_batches: u64,
    pub total_operations: u64,
    pub total_simd_operations: u64,
    pub total_failed: u64,
    pub avg_batch_size: f64,
    pub avg_processing_time_ns: f64,
    pub throughput_ops_per_sec: f64,
    pub session_cache_hits: u64,
    pub session_cache_misses: u64,
    pub buffer_reuse_count: u64,
    pub buffer_allocation_count: u64,
}

impl Default for BatchStats {
    fn default() -> Self {
        Self {
            total_batches: 0,
            total_operations: 0,
            total_simd_operations: 0,
            total_failed: 0,
            avg_batch_size: 0.0,
            avg_processing_time_ns: 0.0,
            throughput_ops_per_sec: 0.0,
            session_cache_hits: 0,
            session_cache_misses: 0,
            buffer_reuse_count: 0,
            buffer_allocation_count: 0,
        }
    }
}