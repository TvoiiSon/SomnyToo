use std::collections::VecDeque;
use std::time::{Instant, Duration};

/// Адаптивный тюнер размера батча
#[derive(Debug, Clone)]
pub struct AdaptiveBatchTuner {
    current_batch_size: usize,
    min_batch_size: usize,
    max_batch_size: usize,
    history: VecDeque<BatchPerformance>,
    learning_rate: f64,
    target_latency: Duration,
}

#[derive(Debug, Clone)]
pub struct BatchPerformance {
    pub batch_size: usize,
    pub processing_time: Duration,
    pub frames_per_second: f64,
    pub timestamp: Instant,
}

impl AdaptiveBatchTuner {
    /// Создание нового тюнера
    pub fn new(
        initial_size: usize,
        min_size: usize,
        max_size: usize,
        target_latency: Duration,
    ) -> Self {
        Self {
            current_batch_size: initial_size,
            min_batch_size: min_size,
            max_batch_size: max_size,
            history: VecDeque::with_capacity(100),
            learning_rate: 0.1,
            target_latency,
        }
    }

    /// Настройка размера батча на основе производительности
    pub fn adjust_batch_size(&mut self, actual_batch_size: usize, processing_time: Duration) -> usize {
        // Сохраняем историю производительности
        let performance = BatchPerformance {
            batch_size: actual_batch_size,
            processing_time,
            frames_per_second: if processing_time.as_secs_f64() > 0.0 {
                actual_batch_size as f64 / processing_time.as_secs_f64()
            } else {
                0.0
            },
            timestamp: Instant::now(),
        };

        self.history.push_back(performance);

        // Ограничиваем историю
        if self.history.len() > 100 {
            self.history.pop_front();
        }

        // Анализируем последние N батчей
        let recent_history: Vec<_> = self.history.iter().rev().take(10).collect();

        if recent_history.is_empty() {
            return self.current_batch_size;
        }

        // Рассчитываем среднюю производительность
        let avg_fps: f64 = recent_history.iter()
            .map(|p| p.frames_per_second)
            .sum::<f64>() / recent_history.len() as f64;

        let avg_latency: Duration = recent_history.iter()
            .map(|p| p.processing_time)
            .sum::<Duration>() / recent_history.len() as u32;

        // Адаптивная настройка
        if avg_latency > self.target_latency * 2 {
            // Слишком большая задержка - уменьшаем размер батча
            self.current_batch_size = (self.current_batch_size as f64 * (1.0 - self.learning_rate))
                .max(self.min_batch_size as f64) as usize;
        } else if avg_latency < self.target_latency / 2 && avg_fps > 1000.0 {
            // Хорошая производительность, можно увеличить
            self.current_batch_size = (self.current_batch_size as f64 * (1.0 + self.learning_rate))
                .min(self.max_batch_size as f64) as usize;
        }

        self.current_batch_size
    }

    /// Получение текущего размера батча
    pub fn current_batch_size(&self) -> usize {
        self.current_batch_size
    }

    /// Сброс истории
    pub fn reset_history(&mut self) {
        self.history.clear();
    }

    /// Получение статистики производительности
    pub fn get_performance_stats(&self) -> Option<(f64, Duration)> {
        if self.history.is_empty() {
            return None;
        }

        let recent_history: Vec<_> = self.history.iter().rev().take(10).collect();
        let avg_fps: f64 = recent_history.iter()
            .map(|p| p.frames_per_second)
            .sum::<f64>() / recent_history.len() as f64;

        let avg_latency: Duration = recent_history.iter()
            .map(|p| p.processing_time)
            .sum::<Duration>() / recent_history.len() as u32;

        Some((avg_fps, avg_latency))
    }
}