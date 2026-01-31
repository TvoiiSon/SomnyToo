use std::time::{Instant, Duration};
use super::priority::BatchPriority;

/// Состояние батча
#[derive(Debug, Clone)]
pub struct BatchState {
    pub id: u64,
    pub priority: BatchPriority,
    pub size: usize,
    pub created_at: Instant,
    pub processing_start: Option<Instant>,
    pub processing_end: Option<Instant>,
    pub status: BatchStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BatchStatus {
    Collecting,
    Processing,
    Completed,
    Failed,
}

impl BatchState {
    /// Создает новое состояние батча
    pub fn new(id: u64, priority: BatchPriority, size: usize) -> Self {
        Self {
            id,
            priority,
            size,
            created_at: Instant::now(),
            processing_start: None,
            processing_end: None,
            status: BatchStatus::Collecting,
        }
    }

    /// Начинает обработку батча
    pub fn start_processing(&mut self) {
        self.processing_start = Some(Instant::now());
        self.status = BatchStatus::Processing;
    }

    /// Завершает обработку батча
    pub fn complete(&mut self, success: bool) {
        self.processing_end = Some(Instant::now());
        self.status = if success {
            BatchStatus::Completed
        } else {
            BatchStatus::Failed
        };
    }

    /// Возвращает время обработки
    pub fn processing_time(&self) -> Option<Duration> {
        if let (Some(start), Some(end)) = (self.processing_start, self.processing_end) {
            Some(end.duration_since(start))
        } else {
            None
        }
    }

    /// Возвращает время жизни батча
    pub fn age(&self) -> Duration {
        Instant::now().duration_since(self.created_at)
    }

    /// Проверяет, истек ли таймаут батча
    pub fn is_timed_out(&self, timeout: Duration) -> bool {
        self.age() > timeout
    }
}