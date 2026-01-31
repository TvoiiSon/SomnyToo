use std::collections::HashMap;
use std::time::Duration;

use super::priority::DispatchPriority;

/// Статистика диспетчера
#[derive(Debug, Default, Clone)]
pub struct DispatcherStats {
    pub total_tasks_received: u64,
    pub total_tasks_processed: u64,
    pub total_batches_processed: u64,
    pub avg_processing_time: Duration,
    pub queue_sizes: Vec<usize>,
    pub worker_loads: Vec<f64>,
    pub task_distribution: HashMap<DispatchPriority, u64>,
    pub throughput_tasks_per_sec: f64,
    pub throughput_batches_per_sec: f64,
    pub work_stealing_events: u64,
    pub backpressure_events: u64,
    pub task_timeouts: u64,
    pub worker_errors: u64,
}

impl DispatcherStats {
    /// Создание новой статистики
    pub fn new() -> Self {
        Self::default()
    }

    /// Обновление статистики при получении задачи
    pub fn task_received(&mut self, priority: DispatchPriority) {
        self.total_tasks_received += 1;
        *self.task_distribution.entry(priority).or_insert(0) += 1;
    }

    /// Обновление статистики при обработке задачи
    pub fn task_processed(&mut self, processing_time: Duration) {
        self.total_tasks_processed += 1;

        // Обновляем среднее время обработки
        let total_tasks = self.total_tasks_processed as f64;
        let current_avg = self.avg_processing_time.as_nanos() as f64;
        let new_avg = (current_avg * (total_tasks - 1.0) + processing_time.as_nanos() as f64) / total_tasks;
        self.avg_processing_time = Duration::from_nanos(new_avg as u64);
    }

    /// Обновление статистики при обработке батча
    pub fn batch_processed(&mut self, batch_size: usize, processing_time: Duration) {
        self.total_batches_processed += 1;

        // Обновляем throughput
        if processing_time.as_micros() > 0 {
            let tps = batch_size as f64 / (processing_time.as_micros() as f64 / 1_000_000.0);
            self.throughput_tasks_per_sec = 0.7 * self.throughput_tasks_per_sec + 0.3 * tps;
            self.throughput_batches_per_sec = 1.0 / processing_time.as_secs_f64();
        }
    }

    /// Обновление размеров очередей
    pub fn update_queue_sizes(&mut self, sizes: Vec<usize>) {
        self.queue_sizes = sizes;
    }

    /// Обновление нагрузки worker-ов
    pub fn update_worker_loads(&mut self, loads: Vec<f64>) {
        self.worker_loads = loads;
    }

    /// Регистрация события work stealing
    pub fn work_stealing_event(&mut self) {
        self.work_stealing_events += 1;
    }

    /// Регистрация события backpressure
    pub fn backpressure_event(&mut self) {
        self.backpressure_events += 1;
    }

    /// Регистрация таймаута задачи
    pub fn task_timeout(&mut self) {
        self.task_timeouts += 1;
    }

    /// Регистрация ошибки worker-а
    pub fn worker_error(&mut self) {
        self.worker_errors += 1;
    }

    /// Получение hit rate (отношение обработанных к полученным)
    pub fn hit_rate(&self) -> f64 {
        if self.total_tasks_received > 0 {
            self.total_tasks_processed as f64 / self.total_tasks_received as f64
        } else {
            0.0
        }
    }

    /// Получение среднего размера батча
    pub fn avg_batch_size(&self) -> f64 {
        if self.total_batches_processed > 0 {
            self.total_tasks_processed as f64 / self.total_batches_processed as f64
        } else {
            0.0
        }
    }

    /// Получение статистики по приоритетам в виде строки
    pub fn priority_stats_string(&self) -> String {
        let mut result = String::new();
        for priority in DispatchPriority::all_priorities() {
            let count = self.task_distribution.get(&priority).copied().unwrap_or(0);
            if count > 0 {
                result.push_str(&format!("{:?}: {}, ", priority, count));
            }
        }
        if result.ends_with(", ") {
            result.truncate(result.len() - 2);
        }
        result
    }

    /// Сброс статистики
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}