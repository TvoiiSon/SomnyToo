use std::sync::Arc;
use std::time::{Instant, Duration};
use tokio::sync::{RwLock, Mutex};
use tracing::info;
use dashmap::DashMap;

/// Адаптивный батчер с предсказательной адаптацией на основе ML
pub struct AdaptiveBatcher {
    config: AdaptiveBatcherConfig,
    current_batch_size: RwLock<usize>,
    metrics: RwLock<BatchMetrics>,
    window_metrics: Mutex<Vec<WindowMetric>>,
    metrics_store: Arc<DashMap<String, f64>>,

    // Новые поля для ML-предсказания
    prediction_model: Mutex<PredictionModel>,
    workload_predictor: Mutex<WorkloadPredictor>,
    adaptation_history: Mutex<Vec<AdaptationRecord>>,
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
    pub enable_predictive_adaptation: bool,
    pub prediction_horizon: Duration,
    pub smoothing_factor: f64,
    pub confidence_threshold: f64,
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

    // Новые метрики для ML
    pub prediction_accuracy: f64,
    pub proactive_adaptations: u64,
    pub reactive_adaptations: u64,
    pub avg_prediction_error: f64,
}

#[derive(Debug, Clone)]
pub struct WindowMetric {
    pub timestamp: Instant,
    pub batch_size: usize,
    pub processing_time: Duration,
    pub success_rate: f64,
    pub queue_depth: usize,
    pub system_load: f64,
    pub throughput: f64,
}

/// Модель предсказания на основе двойного экспоненциального сглаживания (Хольта-Винтерса)
#[derive(Debug, Clone)]
struct PredictionModel {
    alpha: f64,          // Коэффициент сглаживания уровня
    beta: f64,           // Коэффициент сглаживания тренда
    gamma: f64,          // Коэффициент сглаживания сезонности
    level: f64,          // Текущий уровень
    trend: f64,          // Текущий тренд
    seasonal: Vec<f64>,  // Сезонные компоненты
    season_length: usize,
    confidence: f64,     // Уверенность предсказания
}

/// Предиктор рабочей нагрузки
#[derive(Debug, Clone)]
struct WorkloadPredictor {
    queue_depth_history: VecDeque<f64>,
    latency_history: VecDeque<f64>,
    throughput_history: VecDeque<f64>,
    pattern_detector: WorkloadPatternDetector,
    prediction_window: usize,
}

#[derive(Debug, Clone)]
struct WorkloadPatternDetector {
    burst_detected: bool,
    steady_state: bool,
    increasing_trend: bool,
    decreasing_trend: bool,
    pattern_confidence: f64,
}

/// Запись истории адаптации
#[derive(Debug, Clone)]
pub struct AdaptationRecord {
    pub timestamp: Instant,
    pub old_size: usize,
    pub new_size: usize,
    pub predicted_improvement: f64,
    pub actual_improvement: f64,
    pub prediction_error: f64,
    pub adaptation_type: AdaptationType,
    pub workload_pattern: WorkloadPattern,
}

/// Тип адаптации
#[derive(Debug, Clone, PartialEq)]
pub enum AdaptationType {
    Proactive,  // Предсказательная адаптация
    Reactive,   // Реактивная адаптация
    Corrective, // Корректирующая адаптация
}

/// Паттерн рабочей нагрузки
#[derive(Debug, Clone)]
pub enum WorkloadPattern {
    Bursty,        // Всплеск нагрузки
    Steady,        // Стабильная нагрузка
    Increasing,    // Растущая нагрузка
    Decreasing,    // Падающая нагрузка
    Periodic,      // Периодическая нагрузка
}

/// Предсказание нагрузки
#[derive(Debug, Clone)]
pub struct LoadPrediction {
    pub predicted_queue: f64,
    pub predicted_latency: f64,
    pub queue_trend: f64,
    pub latency_trend: f64,
    pub confidence: f64,
    pub pattern: WorkloadPattern,
    pub horizon: usize,
}

impl Default for LoadPrediction {
    fn default() -> Self {
        Self {
            predicted_queue: 0.0,
            predicted_latency: 0.0,
            queue_trend: 0.0,
            latency_trend: 0.0,
            confidence: 0.5,
            pattern: WorkloadPattern::Steady,
            horizon: 0,
        }
    }
}

impl PredictionModel {
    fn new(alpha: f64, beta: f64, gamma: f64, season_length: usize) -> Self {
        Self {
            alpha,
            beta,
            gamma,
            level: 0.0,
            trend: 0.0,
            seasonal: vec![0.0; season_length],
            season_length,
            confidence: 0.5,
        }
    }

    /// Обновление модели с новым наблюдением
    fn update(&mut self, observation: f64, timestamp: Instant) -> f64 {
        let season_index = (timestamp.elapsed().as_secs() as usize) % self.season_length;

        // Предсказание
        let prediction = self.level + self.trend + self.seasonal[season_index];
        let error = observation - prediction;

        // Обновление компонентов
        let last_level = self.level;
        self.level = self.alpha * (observation - self.seasonal[season_index])
            + (1.0 - self.alpha) * (self.level + self.trend);
        self.trend = self.beta * (self.level - last_level)
            + (1.0 - self.beta) * self.trend;
        self.seasonal[season_index] = self.gamma * (observation - last_level - self.trend)
            + (1.0 - self.gamma) * self.seasonal[season_index];

        // Обновление уверенности
        let error_abs = error.abs();
        self.confidence = 0.9 * self.confidence + 0.1 * (1.0 - error_abs / (observation + 1.0));

        prediction
    }

    /// Предсказание на горизонт
    fn predict(&self, horizon: usize, timestamp: Instant) -> Vec<f64> {
        let mut predictions = Vec::with_capacity(horizon);
        let base_time = timestamp.elapsed().as_secs() as usize;

        for i in 1..=horizon {
            let season_index = (base_time + i) % self.season_length;
            let prediction = self.level + (self.trend * i as f64) + self.seasonal[season_index];
            predictions.push(prediction);
        }

        predictions
    }
}

impl WorkloadPredictor {
    fn new(prediction_window: usize) -> Self {
        Self {
            queue_depth_history: VecDeque::with_capacity(prediction_window * 2),
            latency_history: VecDeque::with_capacity(prediction_window * 2),
            throughput_history: VecDeque::with_capacity(prediction_window * 2),
            pattern_detector: WorkloadPatternDetector::new(),
            prediction_window,
        }
    }

    fn add_observation(&mut self, queue_depth: f64, latency: f64, throughput: f64) {
        self.queue_depth_history.push_back(queue_depth);
        self.latency_history.push_back(latency);
        self.throughput_history.push_back(throughput);

        if self.queue_depth_history.len() > self.prediction_window * 2 {
            self.queue_depth_history.pop_front();
            self.latency_history.pop_front();
            self.throughput_history.pop_front();
        }

        self.pattern_detector.update(
            &self.queue_depth_history,
            &self.latency_history,
        );
    }

    /// Предсказание будущей нагрузки
    fn predict_load(&self, horizon: usize) -> LoadPrediction {
        if self.queue_depth_history.len() < 10 {
            return LoadPrediction::default();
        }

        // Простое линейное предсказание на основе скользящего среднего
        let recent_queue: Vec<f64> = self.queue_depth_history.iter().copied().collect();
        let recent_latency: Vec<f64> = self.latency_history.iter().copied().collect();

        let queue_trend = Self::calculate_trend(&recent_queue);
        let latency_trend = Self::calculate_trend(&recent_latency);

        let current_queue = recent_queue.last().copied().unwrap_or(0.0);
        let current_latency = recent_latency.last().copied().unwrap_or(0.0);

        let predicted_queue = current_queue + queue_trend * horizon as f64;
        let predicted_latency = current_latency + latency_trend * horizon as f64;

        let confidence = self.pattern_detector.pattern_confidence;
        let pattern = self.pattern_detector.detect_pattern();

        LoadPrediction {
            predicted_queue,
            predicted_latency,
            queue_trend,
            latency_trend,
            confidence,
            pattern,
            horizon,
        }
    }

    fn calculate_trend(data: &[f64]) -> f64 {
        if data.len() < 2 {
            return 0.0;
        }

        let n = data.len() as f64;
        let sum_x: f64 = (0..data.len()).map(|i| i as f64).sum();
        let sum_y: f64 = data.iter().sum();
        let sum_xy: f64 = data.iter().enumerate().map(|(i, &y)| i as f64 * y).sum();
        let sum_x2: f64 = (0..data.len()).map(|i| (i as f64).powi(2)).sum();

        let numerator = n * sum_xy - sum_x * sum_y;
        let denominator = n * sum_x2 - sum_x * sum_x;

        if denominator.abs() > 1e-10 {
            numerator / denominator
        } else {
            0.0
        }
    }
}

impl WorkloadPatternDetector {
    fn new() -> Self {
        Self {
            burst_detected: false,
            steady_state: false,
            increasing_trend: false,
            decreasing_trend: false,
            pattern_confidence: 0.5,
        }
    }

    fn update(&mut self, queue_history: &VecDeque<f64>, latency_history: &VecDeque<f64>) {
        if queue_history.len() < 5 {
            return;
        }

        let queue_data: Vec<f64> = queue_history.iter().copied().collect();
        let latency_data: Vec<f64> = latency_history.iter().copied().collect();

        // Обнаружение всплесков (burst)
        let queue_mean: f64 = queue_data.iter().sum::<f64>() / queue_data.len() as f64;
        let queue_std = Self::calculate_std(&queue_data, queue_mean);
        let last_queue = *queue_data.last().unwrap();

        self.burst_detected = last_queue > queue_mean + 2.0 * queue_std;

        // Обнаружение трендов
        let queue_trend = WorkloadPredictor::calculate_trend(&queue_data);
        let latency_trend = WorkloadPredictor::calculate_trend(&latency_data);

        self.increasing_trend = queue_trend > 0.1 || latency_trend > 0.05;
        self.decreasing_trend = queue_trend < -0.1 || latency_trend < -0.05;
        self.steady_state = !self.burst_detected && !self.increasing_trend && !self.decreasing_trend;

        // Расчет уверенности
        let burst_confidence = if self.burst_detected { 0.8 } else { 0.2 };
        let trend_confidence = queue_trend.abs().min(1.0);
        self.pattern_confidence = 0.7 * burst_confidence + 0.3 * trend_confidence;
    }

    fn calculate_std(data: &[f64], mean: f64) -> f64 {
        let variance: f64 = data.iter()
            .map(|&x| (x - mean).powi(2))
            .sum::<f64>() / data.len() as f64;
        variance.sqrt()
    }

    fn detect_pattern(&self) -> WorkloadPattern {
        if self.burst_detected {
            WorkloadPattern::Bursty
        } else if self.increasing_trend {
            WorkloadPattern::Increasing
        } else if self.decreasing_trend {
            WorkloadPattern::Decreasing
        } else if self.steady_state {
            WorkloadPattern::Steady
        } else {
            // Проверяем на периодичность (упрощенная проверка)
            // В реальной реализации здесь должна быть проверка сезонности
            WorkloadPattern::Steady
        }
    }
}

impl AdaptiveBatcher {
    pub fn new(config: AdaptiveBatcherConfig) -> Self {
        info!("🔄 AdaptiveBatcher инициализирован с предсказательной адаптацией");

        let prediction_model = PredictionModel::new(
            config.smoothing_factor,
            config.smoothing_factor * 0.5,
            config.smoothing_factor * 0.3,
            60, // Сезонность 60 секунд
        );

        let workload_predictor = WorkloadPredictor::new(
            config.prediction_horizon.as_secs() as usize
        );

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
                prediction_accuracy: 0.5,
                proactive_adaptations: 0,
                reactive_adaptations: 0,
                avg_prediction_error: 0.0,
            }),
            window_metrics: Mutex::new(Vec::new()),
            metrics_store: Arc::new(DashMap::new()),
            prediction_model: Mutex::new(prediction_model),
            workload_predictor: Mutex::new(workload_predictor),
            adaptation_history: Mutex::new(Vec::new()),
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
        let processing_time_ms = processing_time.as_millis() as f64;
        let throughput = batch_size as f64 / (processing_time.as_secs_f64() + 0.001);

        // Рассчитываем загрузку системы
        let system_load = (queue_depth as f64 / self.config.max_batch_size as f64)
            .min(1.0);

        // Обновляем метрики окна
        {
            let mut window = self.window_metrics.lock().await;
            window.push(WindowMetric {
                timestamp: now,
                batch_size,
                processing_time,
                success_rate,
                queue_depth,
                system_load,
                throughput,
            });

            // Удаляем старые метрики
            window.retain(|m| now.duration_since(m.timestamp) <= self.config.window_duration);
        }

        // Обновляем предсказательные модели
        {
            let mut predictor = self.workload_predictor.lock().await;
            predictor.add_observation(
                queue_depth as f64,
                processing_time_ms,
                throughput,
            );

            let mut model = self.prediction_model.lock().await;
            model.update(processing_time_ms, now);
        }

        // Обновляем общие метрики
        {
            let mut metrics = self.metrics.write().await;
            metrics.total_batches += 1;
            metrics.total_items += batch_size as u64;

            // Экспоненциальное сглаживание метрик
            let alpha = 0.1;
            metrics.avg_batch_size = metrics.avg_batch_size * (1.0 - alpha) + batch_size as f64 * alpha;
            metrics.avg_processing_time = Duration::from_nanos(
                (metrics.avg_processing_time.as_nanos() as f64 * (1.0 - alpha) +
                    processing_time.as_nanos() as f64 * alpha) as u64
            );
        }

        // Записываем метрики в хранилище
        self.record_metric("batch_size".to_string(), batch_size as f64);
        self.record_metric("processing_time_ms".to_string(), processing_time_ms);
        self.record_metric("success_rate".to_string(), success_rate);
        self.record_metric("queue_depth".to_string(), queue_depth as f64);
        self.record_metric("system_load".to_string(), system_load);
        self.record_metric("throughput".to_string(), throughput);

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

        // Выполняем адаптацию с предсказанием
        self.perform_predictive_adaptation().await;
    }

    /// Выполнить предсказательную адаптацию
    async fn perform_predictive_adaptation(&self) {
        let window_metrics = self.window_metrics.lock().await.clone();

        if window_metrics.is_empty() {
            return;
        }

        // Получаем текущие метрики
        let current_batch_size = *self.current_batch_size.read().await;
        let target_latency_ns = self.config.target_latency.as_nanos() as f64;

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

        let avg_system_load = window_metrics.iter()
            .map(|m| m.system_load)
            .sum::<f64>() / window_metrics.len() as f64;

        // Получаем предсказание нагрузки
        let load_prediction = {
            let predictor = self.workload_predictor.lock().await;
            let horizon = self.config.prediction_horizon.as_secs() as usize / 5; // 5-секундные интервалы
            predictor.predict_load(horizon.min(12)) // Максимум 12 интервалов (1 минута)
        };

        // Получаем предсказание задержки
        let latency_prediction = {
            let model = self.prediction_model.lock().await;
            let horizon = self.config.prediction_horizon.as_secs() as usize / 5;
            model.predict(horizon.min(12), Instant::now())
        };

        // Определяем тип адаптации на основе уверенности предсказания
        let adaptation_type = if load_prediction.confidence > self.config.confidence_threshold &&
            self.config.enable_predictive_adaptation {
            AdaptationType::Proactive
        } else if avg_processing_time > target_latency_ns * 1.3 {
            AdaptationType::Reactive
        } else {
            AdaptationType::Corrective
        };

        // Выбираем стратегию адаптации
        let new_batch_size = match adaptation_type {
            AdaptationType::Proactive => {
                self.perform_proactive_adaptation(
                    current_batch_size,
                    &load_prediction,
                    &latency_prediction,
                    avg_system_load,
                )
            }
            AdaptationType::Reactive => {
                self.perform_reactive_adaptation(
                    current_batch_size,
                    avg_processing_time,
                    target_latency_ns,
                    avg_success_rate,
                    avg_queue_depth,
                )
            }
            AdaptationType::Corrective => {
                self.perform_corrective_adaptation(
                    current_batch_size,
                    avg_processing_time,
                    target_latency_ns,
                    avg_success_rate,
                    avg_system_load,
                )
            }
        };

        // Применяем изменение
        if new_batch_size != current_batch_size {
            let old_size = current_batch_size;
            *self.current_batch_size.write().await = new_batch_size;

            let mut metrics = self.metrics.write().await;
            metrics.last_adaptation = Instant::now();
            metrics.adaptation_count += 1;

            // Обновляем статистику адаптаций
            match adaptation_type {
                AdaptationType::Proactive => metrics.proactive_adaptations += 1,
                AdaptationType::Reactive => metrics.reactive_adaptations += 1,
                _ => {}
            }

            // Записываем историю адаптации
            let predicted_improvement = self.calculate_predicted_improvement(
                old_size,
                new_batch_size,
                avg_processing_time,
                &load_prediction,
            );

            self.record_adaptation_history(
                old_size,
                new_batch_size,
                predicted_improvement,
                avg_processing_time,
                adaptation_type.clone(),
                load_prediction.pattern.clone(),
            ).await;

            info!("🔄 AdaptiveBatcher: {} адаптация с {} на {} (задержка: {:.1}мс, уверенность: {:.1}%, паттерн: {:?})",
                  match adaptation_type {
                      AdaptationType::Proactive => "Предсказательная",
                      AdaptationType::Reactive => "Реактивная",
                      AdaptationType::Corrective => "Корректирующая",
                  },
                  old_size, new_batch_size,
                  avg_processing_time / 1_000_000.0,
                  load_prediction.confidence * 100.0,
                  load_prediction.pattern);

            self.record_metric("adapted_batch_size".to_string(), new_batch_size as f64);
            self.record_metric("adaptation_count".to_string(), metrics.adaptation_count as f64);
            self.record_metric("adaptation_type".to_string(),
                               match adaptation_type {
                                   AdaptationType::Proactive => 0.0,
                                   AdaptationType::Reactive => 1.0,
                                   AdaptationType::Corrective => 2.0,
                               });
            self.record_metric("prediction_confidence".to_string(), load_prediction.confidence);
        }
    }

    /// Предсказательная адаптация
    fn perform_proactive_adaptation(
        &self,
        current_size: usize,
        load_prediction: &LoadPrediction,
        latency_prediction: &[f64],
        current_load: f64,
    ) -> usize {
        let predicted_latency = load_prediction.predicted_latency;
        let _predicted_queue = load_prediction.predicted_queue; // Используется для логики принятия решений
        let confidence = load_prediction.confidence;

        // Адаптивная стратегия на основе предсказания
        let mut new_size = current_size;

        match &load_prediction.pattern {
            WorkloadPattern::Bursty => {
                // При всплеске нагрузки уменьшаем размер батча для быстрой реакции
                let reduction_factor = 1.0 - (0.3 * confidence).min(0.5);
                new_size = (current_size as f64 * reduction_factor).ceil() as usize;
            }
            WorkloadPattern::Increasing => {
                // При растущей нагрузке постепенно уменьшаем размер
                let trend_factor = load_prediction.queue_trend.abs().min(0.5);
                let reduction = 1.0 - (0.2 * trend_factor * confidence);
                new_size = (current_size as f64 * reduction).ceil() as usize;
            }
            WorkloadPattern::Decreasing => {
                // При падающей нагрузке можно увеличить размер
                let trend_factor = load_prediction.queue_trend.abs().min(0.3);
                let increase = 1.0 + (0.25 * trend_factor * confidence);
                new_size = (current_size as f64 * increase).ceil() as usize;
            }
            WorkloadPattern::Steady => {
                // Стабильная нагрузка - оптимизируем на основе предсказанной задержки
                if predicted_latency > self.config.target_latency.as_millis() as f64 * 1.1 {
                    let reduction = (predicted_latency / self.config.target_latency.as_millis() as f64)
                        .min(2.0);
                    new_size = (current_size as f64 / reduction).ceil() as usize;
                } else if predicted_latency < self.config.target_latency.as_millis() as f64 * 0.7 {
                    let increase = 1.0 + (self.config.max_increase_rate * confidence).min(0.4);
                    new_size = (current_size as f64 * increase).ceil() as usize;
                }
            }
            WorkloadPattern::Periodic => {
                // Периодическая нагрузка - используем сезонность
                if let Some(&next_latency) = latency_prediction.first() {
                    if next_latency > self.config.target_latency.as_millis() as f64 {
                        new_size = (current_size as f64 * 0.8).ceil() as usize;
                    }
                }
            }
        }

        // Учитываем текущую загрузку системы
        if current_load > 0.8 {
            new_size = (new_size as f64 * 0.9).ceil() as usize;
        }

        new_size.max(self.config.min_batch_size).min(self.config.max_batch_size)
    }

    /// Реактивная адаптация (оригинальная логика)
    fn perform_reactive_adaptation(
        &self,
        current_size: usize,
        avg_processing_time: f64,
        target_latency_ns: f64,
        avg_success_rate: f64,
        avg_queue_depth: f64,
    ) -> usize {
        if avg_processing_time > target_latency_ns * 1.2 {
            // Слишком большая задержка - уменьшаем размер батча
            let decrease_factor = (avg_processing_time / target_latency_ns).min(2.0);
            let new_size = (current_size as f64 / decrease_factor).ceil() as usize;
            new_size.max(self.config.min_batch_size)
        } else if avg_processing_time < target_latency_ns * 0.8 && avg_success_rate > 0.95 {
            // Задержка низкая, успешность высокая - можно увеличить
            let increase_factor = 1.0 + self.config.max_increase_rate.min(0.5);
            let new_size = (current_size as f64 * increase_factor).ceil() as usize;
            new_size.min(self.config.max_batch_size)
        } else if avg_queue_depth > (current_size * 2) as f64 {
            // Большая очередь - увеличиваем размер батча
            let increase_factor = 1.0 + (avg_queue_depth / current_size as f64).min(0.3);
            let new_size = (current_size as f64 * increase_factor).ceil() as usize;
            new_size.min(self.config.max_batch_size)
        } else {
            // Оставляем текущий размер
            current_size
        }
    }

    /// Корректирующая адаптация
    fn perform_corrective_adaptation(
        &self,
        current_size: usize,
        avg_processing_time: f64,
        target_latency_ns: f64,
        avg_success_rate: f64,
        avg_system_load: f64,
    ) -> usize {
        let latency_ratio = avg_processing_time / target_latency_ns;

        if latency_ratio > 1.1 && avg_success_rate < 0.9 {
            // Небольшое превышение задержки с низкой успешностью
            let reduction = 1.0 - (0.1 * (latency_ratio - 1.0)).min(0.2);
            (current_size as f64 * reduction).ceil() as usize
        } else if latency_ratio < 0.9 && avg_success_rate > 0.98 && avg_system_load < 0.7 {
            // Хорошие показатели, можно оптимизировать
            let increase = 1.0 + (0.1 * (1.0 - latency_ratio)).min(0.15);
            (current_size as f64 * increase).ceil() as usize
        } else {
            current_size
        }.max(self.config.min_batch_size).min(self.config.max_batch_size)
    }

    fn calculate_predicted_improvement(
        &self,
        old_size: usize,
        new_size: usize,
        _current_latency: f64,
        load_prediction: &LoadPrediction,
    ) -> f64 {
        if old_size == new_size {
            return 0.0;
        }

        let size_ratio = new_size as f64 / old_size as f64;
        let latency_trend = load_prediction.latency_trend;

        // Ожидаемое улучшение основано на изменении размера и тренде задержки
        let size_effect = if size_ratio < 1.0 {
            // Уменьшение размера должно уменьшить задержку
            1.0 / size_ratio - 1.0
        } else {
            // Увеличение размера может увеличить задержку
            -(size_ratio - 1.0) * 0.5
        };

        let trend_effect = -latency_trend * 10.0; // Компенсация тренда

        (size_effect + trend_effect) * load_prediction.confidence
    }

    async fn record_adaptation_history(
        &self,
        old_size: usize,
        new_size: usize,
        predicted_improvement: f64,
        _current_processing_time: f64,
        adaptation_type: AdaptationType,
        workload_pattern: WorkloadPattern,
    ) {
        let mut history = self.adaptation_history.lock().await;

        let record = AdaptationRecord {
            timestamp: Instant::now(),
            old_size,
            new_size,
            predicted_improvement,
            actual_improvement: 0.0, // Будет обновлено при следующей адаптации
            prediction_error: 0.0,
            adaptation_type,
            workload_pattern,
        };

        // Обновляем actual_improvement в предыдущей записи, если она существует
        if let Some(prev_record) = history.last_mut() {
            if prev_record.actual_improvement == 0.0 {
                // Вычисляем фактическое улучшение на основе изменения размера батча
                let size_change_percentage = if prev_record.old_size == 0 {
                    0.0
                } else {
                    (new_size as f64 / prev_record.old_size as f64 - 1.0) * 100.0
                };

                // Простое вычисление улучшения: отрицательное изменение размера = улучшение
                let actual_improvement = -size_change_percentage / 100.0;

                prev_record.actual_improvement = actual_improvement.max(-1.0).min(1.0);
                prev_record.prediction_error = (actual_improvement - prev_record.predicted_improvement).abs();
            }
        }

        history.push(record);

        // Ограничиваем размер истории
        if history.len() > 100 {
            history.remove(0);
        }

        // Обновляем точность предсказаний на основе истории
        self.update_prediction_accuracy().await;
    }

    async fn update_prediction_accuracy(&self) {
        let history = self.adaptation_history.lock().await;

        if history.len() < 5 {
            return;
        }

        let mut total_error = 0.0;
        let mut count = 0;

        for record in history.iter() {
            if record.prediction_error > 0.0 {
                total_error += record.prediction_error;
                count += 1;
            }
        }

        if count > 0 {
            let avg_error = total_error / count as f64;
            let accuracy = 1.0 - avg_error.min(1.0);

            let mut metrics = self.metrics.write().await;
            metrics.prediction_accuracy = accuracy;
            metrics.avg_prediction_error = avg_error;

            self.record_metric("prediction_accuracy".to_string(), accuracy);
            self.record_metric("avg_prediction_error".to_string(), avg_error);
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

    /// Получить историю адаптаций
    pub async fn get_adaptation_history(&self, limit: usize) -> Vec<AdaptationRecord> {
        let history = self.adaptation_history.lock().await;
        let start = if history.len() > limit {
            history.len() - limit
        } else {
            0
        };
        history[start..].to_vec()
    }

    /// Получить предсказание нагрузки
    pub async fn get_load_prediction(&self, horizon_seconds: u64) -> LoadPrediction {
        let predictor = self.workload_predictor.lock().await;
        let horizon = (horizon_seconds / 5).max(1) as usize; // 5-секундные интервалы
        predictor.predict_load(horizon.min(60)) // Максимум 60 интервалов (5 минут)
    }

    /// Принудительно выполнить адаптацию
    pub async fn force_adaptation(&self) {
        self.perform_predictive_adaptation().await;
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
            prediction_accuracy: 0.5,
            proactive_adaptations: 0,
            reactive_adaptations: 0,
            avg_prediction_error: 0.0,
        };

        self.window_metrics.lock().await.clear();
        self.metrics_store.clear();

        // Сброс моделей предсказания
        *self.prediction_model.lock().await = PredictionModel::new(
            self.config.smoothing_factor,
            self.config.smoothing_factor * 0.5,
            self.config.smoothing_factor * 0.3,
            60,
        );

        *self.workload_predictor.lock().await = WorkloadPredictor::new(
            self.config.prediction_horizon.as_secs() as usize
        );

        self.adaptation_history.lock().await.clear();

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
            enable_predictive_adaptation: true,
            prediction_horizon: Duration::from_secs(30), // 30-секундный горизонт предсказания
            smoothing_factor: 0.3,                      // Коэффициент сглаживания
            confidence_threshold: 0.7,                   // Порог уверенности для предсказательной адаптации
        }
    }
}

// Добавляем недостающий импорт
use std::collections::VecDeque;