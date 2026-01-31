use std::time::Duration;

use crate::core::protocol::phantom_crypto::batch::types::priority::BatchPriority;

/// Конфигурация оркестратора
#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    pub max_batch_size: usize,
    pub max_queue_size: usize,
    pub flush_intervals: Vec<(BatchPriority, Duration)>,
    pub max_processing_time: Duration,
    pub enable_auto_flush: bool,
    pub backpressure_threshold: usize,
    pub worker_count: usize,
    pub emergency_flush_threshold: usize,
    pub batch_timeout: Duration,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        let mut flush_intervals = Vec::new();
        flush_intervals.push((BatchPriority::Realtime, Duration::from_millis(10)));
        flush_intervals.push((BatchPriority::High, Duration::from_millis(50)));
        flush_intervals.push((BatchPriority::Normal, Duration::from_millis(100)));
        flush_intervals.push((BatchPriority::Low, Duration::from_millis(500)));
        flush_intervals.push((BatchPriority::Background, Duration::from_millis(1000)));

        Self {
            max_batch_size: 128,
            max_queue_size: 10000,
            flush_intervals,
            max_processing_time: Duration::from_secs(5),
            enable_auto_flush: true,
            backpressure_threshold: 5000,
            worker_count: 4,
            emergency_flush_threshold: 8000,
            batch_timeout: Duration::from_secs(30),
        }
    }
}

impl OrchestratorConfig {
    /// Получение интервала flush для определенного приоритета
    pub fn get_flush_interval(&self, priority: BatchPriority) -> Duration {
        self.flush_intervals
            .iter()
            .find(|(p, _)| *p == priority)
            .map(|(_, interval)| *interval)
            .unwrap_or_else(|| {
                // Значение по умолчанию на основе приоритета
                match priority {
                    BatchPriority::Realtime => Duration::from_millis(10),
                    BatchPriority::High => Duration::from_millis(50),
                    BatchPriority::Normal => Duration::from_millis(100),
                    BatchPriority::Low => Duration::from_millis(500),
                    BatchPriority::Background => Duration::from_millis(1000),
                }
            })
    }

    /// Создание конфигурации для высокопроизводительного режима
    pub fn high_performance() -> Self {
        let mut flush_intervals = Vec::new();
        flush_intervals.push((BatchPriority::Realtime, Duration::from_millis(5)));
        flush_intervals.push((BatchPriority::High, Duration::from_millis(20)));
        flush_intervals.push((BatchPriority::Normal, Duration::from_millis(50)));
        flush_intervals.push((BatchPriority::Low, Duration::from_millis(200)));
        flush_intervals.push((BatchPriority::Background, Duration::from_millis(500)));

        Self {
            max_batch_size: 256,
            max_queue_size: 20000,
            flush_intervals,
            max_processing_time: Duration::from_secs(2),
            enable_auto_flush: true,
            backpressure_threshold: 10000,
            worker_count: 8,
            emergency_flush_threshold: 15000,
            batch_timeout: Duration::from_secs(15),
        }
    }

    /// Создание конфигурации для режима низкой задержки
    pub fn low_latency() -> Self {
        let mut flush_intervals = Vec::new();
        flush_intervals.push((BatchPriority::Realtime, Duration::from_millis(1)));
        flush_intervals.push((BatchPriority::High, Duration::from_millis(5)));
        flush_intervals.push((BatchPriority::Normal, Duration::from_millis(20)));
        flush_intervals.push((BatchPriority::Low, Duration::from_millis(100)));
        flush_intervals.push((BatchPriority::Background, Duration::from_millis(250)));

        Self {
            max_batch_size: 64,
            max_queue_size: 5000,
            flush_intervals,
            max_processing_time: Duration::from_millis(500),
            enable_auto_flush: true,
            backpressure_threshold: 2500,
            worker_count: 4,
            emergency_flush_threshold: 4000,
            batch_timeout: Duration::from_secs(5),
        }
    }

    /// Создание конфигурации для режима экономии памяти
    pub fn memory_efficient() -> Self {
        let mut flush_intervals = Vec::new();
        flush_intervals.push((BatchPriority::Realtime, Duration::from_millis(20)));
        flush_intervals.push((BatchPriority::High, Duration::from_millis(100)));
        flush_intervals.push((BatchPriority::Normal, Duration::from_millis(250)));
        flush_intervals.push((BatchPriority::Low, Duration::from_millis(1000)));
        flush_intervals.push((BatchPriority::Background, Duration::from_millis(2000)));

        Self {
            max_batch_size: 32,
            max_queue_size: 2000,
            flush_intervals,
            max_processing_time: Duration::from_secs(10),
            enable_auto_flush: true,
            backpressure_threshold: 1000,
            worker_count: 2,
            emergency_flush_threshold: 1500,
            batch_timeout: Duration::from_secs(60),
        }
    }

    /// Валидация конфигурации
    pub fn validate(&self) -> Result<(), String> {
        if self.max_batch_size == 0 {
            return Err("max_batch_size must be greater than 0".to_string());
        }

        if self.max_queue_size == 0 {
            return Err("max_queue_size must be greater than 0".to_string());
        }

        if self.backpressure_threshold >= self.max_queue_size {
            return Err("backpressure_threshold must be less than max_queue_size".to_string());
        }

        if self.emergency_flush_threshold <= self.backpressure_threshold {
            return Err("emergency_flush_threshold must be greater than backpressure_threshold".to_string());
        }

        if self.worker_count == 0 {
            return Err("worker_count must be greater than 0".to_string());
        }

        Ok(())
    }
}