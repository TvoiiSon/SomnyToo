use std::time::Instant;

/// Статистика использования буферов
#[derive(Debug, Default, Clone)]
pub struct BufferStats {
    pub total_allocated: usize,
    pub currently_used: usize,
    pub allocation_count: u64,
    pub reuse_count: u64,
    pub hit_rate: f64,
    pub avg_buffer_size: f64,
    pub memory_pressure: f64, // 0.0 - 1.0
}

#[derive(Debug, Clone)]
pub struct GlobalBufferStats {
    pub total_memory_allocated: usize,
    pub peak_memory_usage: usize,
    pub total_allocations: u64,
    pub total_reuses: u64,
    pub memory_pressure_alerts: u64,
    pub last_shrink_time: Instant,
}

impl Default for GlobalBufferStats {
    fn default() -> Self {
        Self {
            total_memory_allocated: 0,
            peak_memory_usage: 0,
            total_allocations: 0,
            total_reuses: 0,
            memory_pressure_alerts: 0,
            last_shrink_time: Instant::now(),
        }
    }
}