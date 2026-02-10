use std::sync::Arc;
use std::time::{Instant, Duration};
use tokio::sync::{RwLock, Mutex};
use tracing::info;
use dashmap::DashMap;

/// Адаптивный батчер с динамической настройкой размера
pub struct AdaptiveBatcher {
    config: AdaptiveBatcherConfig,
    current_batch_size: RwLock<usize>,
    metrics: RwLock<BatchMetrics>,
    window_metrics: Mutex<Vec<WindowMetric>>,
    metrics_store: Arc<DashMap<String, f64>>,
}

#[derive(Debug, Clone)]
pub struct AdaptiveBatcherConfig {
    pub min_batch_size: usize,
    pub max_batch_size: usize,
    pub initial_batch_size: usize,
    pub window_duration: Duration,
    pub target_latency: Duration,
    pub max_increase_rate: f64,
    pub min_decrease_rate: f64,
    pub adaptation_interval: Duration,
    pub enable_auto_tuning: bool,
}

#[derive(Debug, Clone)]
pub struct BatchMetrics {
    pub total_batches: u64,
    pub total_items: u64,
    pub avg_batch_size: f64,
    pub avg_processing_time: Duration,
    pub p95_processing_time: Duration,
    pub p99_processing_time: Duration,
    pub last_adaptation: Instant,
    pub adaptation_count: u64,
}

#[derive(Debug, Clone)]
pub struct WindowMetric {
    pub timestamp: Instant,
    pub batch_size: usize,
    pub processing_time: Duration,
    pub success_rate: f64,
    pub queue_depth: usize,
}

impl AdaptiveBatcher {
    pub fn new(config: AdaptiveBatcherConfig) -> Self {
        info!("🔄 AdaptiveBatcher инициализирован с размером окна {:?}", config.window_duration);

        Self {
            config: config.clone(),
            current_batch_size: RwLock::new(config.initial_batch_size),
            metrics: RwLock::new(BatchMetrics {
                total_batches: 0,
                total_items: 0,
                avg_batch_size: config.initial_batch_size as f64,
                avg_processing_time: Duration::from_millis(0),
                p95_processing_time: Duration::from_millis(0),
                p99_processing_time: Duration::from_millis(0),
                last_adaptation: Instant::now(),
                adaptation_count: 0,
            }),
            window_metrics: Mutex::new(Vec::new()),
            metrics_store: Arc::new(DashMap::new()),
        }
    }

    /// Получить текущий размер батча
    pub async fn get_batch_size(&self) -> usize {
        *self.current_batch_size.read().await
    }

    /// Зарегистрировать выполнение батча
    pub async fn record_batch_execution(
        &self,
        batch_size: usize,
        processing_time: Duration,
        success_rate: f64,
        queue_depth: usize,
    ) {
        let now = Instant::now();

        // Обновляем метрики окна
        {
            let mut window = self.window_metrics.lock().await;
            window.push(WindowMetric {
                timestamp: now,
                batch_size,
                processing_time,
                success_rate,
                queue_depth,
            });

            // Удаляем старые метрики
            window.retain(|m| now.duration_since(m.timestamp) <= self.config.window_duration);
        }

        // Обновляем общие метрики
        {
            let mut metrics = self.metrics.write().await;
            metrics.total_batches += 1;
            metrics.total_items += batch_size as u64;

            // Обновляем среднее значение с экспоненциальным сглаживанием
            let alpha = 0.1;
            metrics.avg_batch_size = metrics.avg_batch_size * (1.0 - alpha) + batch_size as f64 * alpha;
            metrics.avg_processing_time = Duration::from_nanos(
                (metrics.avg_processing_time.as_nanos() as f64 * (1.0 - alpha) +
                    processing_time.as_nanos() as f64 * alpha) as u64
            );
        }

        // Записываем метрики в хранилище
        self.record_metric("batch_size".to_string(), batch_size as f64);
        self.record_metric("processing_time_ms".to_string(), processing_time.as_millis() as f64);
        self.record_metric("success_rate".to_string(), success_rate);
        self.record_metric("queue_depth".to_string(), queue_depth as f64);

        // Проверяем необходимость адаптации
        self.check_adaptation().await;
    }

    /// Проверка необходимости адаптации
    async fn check_adaptation(&self) {
        let now = Instant::now();
        let last_adaptation = self.metrics.read().await.last_adaptation;

        if now.duration_since(last_adaptation) < self.config.adaptation_interval {
            return;
        }

        if !self.config.enable_auto_tuning {
            return;
        }

        // Выполняем адаптацию
        self.perform_adaptation().await;
    }

    /// Выполнить адаптацию размера батча
    async fn perform_adaptation(&self) {
        let window_metrics = self.window_metrics.lock().await.clone();

        if window_metrics.is_empty() {
            return;
        }

        // Рассчитываем средние показатели за окно
        let avg_processing_time = window_metrics.iter()
            .map(|m| m.processing_time.as_nanos() as f64)
            .sum::<f64>() / window_metrics.len() as f64;

        let avg_success_rate = window_metrics.iter()
            .map(|m| m.success_rate)
            .sum::<f64>() / window_metrics.len() as f64;

        let avg_queue_depth = window_metrics.iter()
            .map(|m| m.queue_depth as f64)
            .sum::<f64>() / window_metrics.len() as f64;

        let target_latency_ns = self.config.target_latency.as_nanos() as f64;
        let current_batch_size = *self.current_batch_size.read().await;

        // Логика адаптации
        let new_batch_size = if avg_processing_time > target_latency_ns * 1.2 {
            // Слишком большая задержка - уменьшаем размер батча
            let decrease_factor = (avg_processing_time / target_latency_ns).min(2.0);
            let new_size = (current_batch_size as f64 / decrease_factor).ceil() as usize;
            new_size.max(self.config.min_batch_size)
        } else if avg_processing_time < target_latency_ns * 0.8 && avg_success_rate > 0.95 {
            // Задержка низкая, успешность высокая - можно увеличить
            let increase_factor = 1.0 + self.config.max_increase_rate.min(0.5);
            let new_size = (current_batch_size as f64 * increase_factor).ceil() as usize;
            new_size.min(self.config.max_batch_size)
        } else if avg_queue_depth > (current_batch_size * 2) as f64 {
            // Большая очередь - увеличиваем размер батча
            let increase_factor = 1.0 + (avg_queue_depth / current_batch_size as f64).min(0.3);
            let new_size = (current_batch_size as f64 * increase_factor).ceil() as usize;
            new_size.min(self.config.max_batch_size)
        } else {
            // Оставляем текущий размер
            current_batch_size
        };

        // Применяем изменение
        if new_batch_size != current_batch_size {
            *self.current_batch_size.write().await = new_batch_size;

            let mut metrics = self.metrics.write().await;
            metrics.last_adaptation = Instant::now();
            metrics.adaptation_count += 1;

            info!("🔄 AdaptiveBatcher: размер изменен с {} на {} (задержка: {:.1}мс, успешность: {:.1}%, очередь: {})",
                  current_batch_size, new_batch_size,
                  avg_processing_time / 1_000_000.0,
                  avg_success_rate * 100.0,
                  avg_queue_depth as usize);

            self.record_metric("adapted_batch_size".to_string(), new_batch_size as f64);
            self.record_metric("adaptation_count".to_string(), metrics.adaptation_count as f64);
        }
    }

    fn record_metric(&self, key: String, value: f64) {
        self.metrics_store.insert(
            format!("adaptive_batcher.{}", key),
            value
        );
    }

    /// Получить текущие метрики
    pub async fn get_metrics(&self) -> BatchMetrics {
        self.metrics.read().await.clone()
    }

    /// Получить метрики окна
    pub async fn get_window_metrics(&self) -> Vec<WindowMetric> {
        self.window_metrics.lock().await.clone()
    }

    /// Принудительно выполнить адаптацию
    pub async fn force_adaptation(&self) {
        self.perform_adaptation().await;
    }

    /// Сбросить к начальным настройкам
    pub async fn reset(&self) {
        *self.current_batch_size.write().await = self.config.initial_batch_size;

        let mut metrics = self.metrics.write().await;
        *metrics = BatchMetrics {
            total_batches: 0,
            total_items: 0,
            avg_batch_size: self.config.initial_batch_size as f64,
            avg_processing_time: Duration::from_millis(0),
            p95_processing_time: Duration::from_millis(0),
            p99_processing_time: Duration::from_millis(0),
            last_adaptation: Instant::now(),
            adaptation_count: 0,
        };

        self.window_metrics.lock().await.clear();
        self.metrics_store.clear();

        info!("🔄 AdaptiveBatcher сброшен к начальным настройкам");
    }
}

impl Default for AdaptiveBatcherConfig {
    fn default() -> Self {
        Self {
            min_batch_size: 32,
            max_batch_size: 1024,
            initial_batch_size: 128,
            window_duration: Duration::from_secs(5),
            target_latency: Duration::from_millis(50),
            max_increase_rate: 0.5,
            min_decrease_rate: 0.3,
            adaptation_interval: Duration::from_secs(1),
            enable_auto_tuning: true,
        }
    }
}