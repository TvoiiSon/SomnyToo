use std::time::Duration;

/// Статистика оркестратора
#[derive(Debug, Clone)]
pub struct OrchestratorStats {
    pub total_operations: u64,
    pub total_batches: u64,
    pub operations_per_second: f64,
    pub avg_batch_processing_time: Duration,
    pub queue_sizes: Vec<usize>,
    pub backpressure_events: u64,
    pub emergency_flushes: u64,
    pub batch_timeouts: u64,
    pub successful_batches: u64,
    pub failed_batches: u64,
    pub flush_count: u64,
    pub session_registry_size: usize,
    pub active_workers: usize,
    pub memory_usage_bytes: usize,
}

impl Default for OrchestratorStats {
    fn default() -> Self {
        Self {
            total_operations: 0,
            total_batches: 0,
            operations_per_second: 0.0,
            avg_batch_processing_time: Duration::default(),
            queue_sizes: Vec::new(),
            backpressure_events: 0,
            emergency_flushes: 0,
            batch_timeouts: 0,
            successful_batches: 0,
            failed_batches: 0,
            flush_count: 0,
            session_registry_size: 0,
            active_workers: 0,
            memory_usage_bytes: 0,
        }
    }
}

impl OrchestratorStats {
    /// Создание новой статистики
    pub fn new() -> Self {
        Self::default()
    }

    /// Обновление статистики операций
    pub fn update_operations(&mut self, operations_count: usize, processing_time: Duration) {
        self.total_operations += operations_count as u64;
        self.total_batches += 1;

        // Обновляем среднее время обработки батча
        let total_batches = self.total_batches as f64;
        let current_avg = self.avg_batch_processing_time.as_nanos() as f64;
        let new_avg = (current_avg * (total_batches - 1.0) + processing_time.as_nanos() as f64) / total_batches;
        self.avg_batch_processing_time = Duration::from_nanos(new_avg as u64);

        // Обновляем операции в секунду
        if processing_time.as_micros() > 0 {
            let ops_per_sec = operations_count as f64 / (processing_time.as_micros() as f64 / 1_000_000.0);
            // Экспоненциальное скользящее среднее
            self.operations_per_second = 0.7 * self.operations_per_second + 0.3 * ops_per_sec;
        }
    }

    /// Регистрация успешного батча
    pub fn register_successful_batch(&mut self) {
        self.successful_batches += 1;
    }

    /// Регистрация неудачного батча
    pub fn register_failed_batch(&mut self) {
        self.failed_batches += 1;
    }

    /// Регистрация события backpressure
    pub fn register_backpressure_event(&mut self) {
        self.backpressure_events += 1;
    }

    /// Регистрация аварийного сброса
    pub fn register_emergency_flush(&mut self) {
        self.emergency_flushes += 1;
    }

    /// Регистрация таймаута батча
    pub fn register_batch_timeout(&mut self) {
        self.batch_timeouts += 1;
    }

    /// Регистрация flush операции
    pub fn register_flush(&mut self) {
        self.flush_count += 1;
    }

    /// Обновление размеров очередей
    pub fn update_queue_sizes(&mut self, sizes: Vec<usize>) {
        self.queue_sizes = sizes;
    }

    /// Обновление размера регистра сессий
    pub fn update_session_registry_size(&mut self, size: usize) {
        self.session_registry_size = size;
    }

    /// Обновление количества активных worker-ов
    pub fn update_active_workers(&mut self, count: usize) {
        self.active_workers = count;
    }

    /// Обновление использования памяти
    pub fn update_memory_usage(&mut self, bytes: usize) {
        self.memory_usage_bytes = bytes;
    }

    /// Получение hit rate (отношение успешных батчей к общему количеству)
    pub fn success_rate(&self) -> f64 {
        if self.total_batches > 0 {
            self.successful_batches as f64 / self.total_batches as f64
        } else {
            0.0
        }
    }

    /// Получение среднего размера батча
    pub fn avg_batch_size(&self) -> f64 {
        if self.total_batches > 0 {
            self.total_operations as f64 / self.total_batches as f64
        } else {
            0.0
        }
    }

    /// Получение backpressure rate
    pub fn backpressure_rate(&self) -> f64 {
        if self.total_operations > 0 {
            self.backpressure_events as f64 / self.total_operations as f64
        } else {
            0.0
        }
    }

    /// Получение использования памяти в МБ
    pub fn memory_usage_mb(&self) -> f64 {
        self.memory_usage_bytes as f64 / (1024.0 * 1024.0)
    }

    /// Получение статистики в виде строки для логирования
    pub fn to_log_string(&self) -> String {
        format!(
            "Operations: {}, Batches: {}, OPS: {:.1}, Success: {:.1}%, AvgBatch: {:.1}, Backpressure: {:.3}%, Memory: {:.2}MB",
            self.total_operations,
            self.total_batches,
            self.operations_per_second,
            self.success_rate() * 100.0,
            self.avg_batch_size(),
            self.backpressure_rate() * 100.0,
            self.memory_usage_mb()
        )
    }

    /// Сброс статистики
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Получение детальной статистики
    pub fn detailed_stats(&self) -> String {
        format!(
            "Orchestrator Statistics:\n\
             Total Operations: {}\n\
             Total Batches: {}\n\
             Operations/sec: {:.1}\n\
             Avg Batch Time: {:?}\n\
             Avg Batch Size: {:.1}\n\
             Success Rate: {:.1}%\n\
             Backpressure Events: {}\n\
             Emergency Flushes: {}\n\
             Batch Timeouts: {}\n\
             Session Registry: {}\n\
             Active Workers: {}\n\
             Memory Usage: {:.2}MB",
            self.total_operations,
            self.total_batches,
            self.operations_per_second,
            self.avg_batch_processing_time,
            self.avg_batch_size(),
            self.success_rate() * 100.0,
            self.backpressure_events,
            self.emergency_flushes,
            self.batch_timeouts,
            self.session_registry_size,
            self.active_workers,
            self.memory_usage_mb()
        )
    }
}