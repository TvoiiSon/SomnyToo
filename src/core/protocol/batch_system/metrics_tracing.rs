use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::{VecDeque, HashMap};
use tracing::{info, debug, warn, error};
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;

/// Модель распределения метрик
#[derive(Debug, Clone)]
pub struct MetricDistributionModel {
    /// Среднее значение (μ)
    pub mean: f64,

    /// Медиана (50-й перцентиль)
    pub median: f64,

    /// Стандартное отклонение (σ)
    pub std_dev: f64,

    /// Дисперсия (σ²)
    pub variance: f64,

    /// Асимметрия (skewness)
    pub skewness: f64,

    /// Эксцесс (kurtosis)
    pub kurtosis: f64,

    /// Минимальное значение
    pub min: f64,

    /// Максимальное значение
    pub max: f64,

    /// Количество наблюдений
    pub count: u64,

    /// Сумма значений
    pub sum: f64,

    /// Сумма квадратов (для онлайн-вычисления дисперсии)
    pub sum_squares: f64,
}

impl MetricDistributionModel {
    pub fn new() -> Self {
        Self {
            mean: 0.0,
            median: 0.0,
            std_dev: 0.0,
            variance: 0.0,
            skewness: 0.0,
            kurtosis: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            count: 0,
            sum: 0.0,
            sum_squares: 0.0,
        }
    }

    /// Обновление статистики онлайн (алгоритм Уэлфорда)
    pub fn update(&mut self, value: f64) {
        self.count += 1;
        self.sum += value;
        self.sum_squares += value * value;

        // Обновление минимума и максимума
        self.min = self.min.min(value);
        self.max = self.max.max(value);

        // Обновление среднего
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;

        // Обновление суммы квадратов отклонений (для дисперсии)
        let delta2 = value - self.mean;
        self.sum_squares = self.sum_squares + delta * delta2;

        // Расчёт дисперсии и стандартного отклонения
        if self.count > 1 {
            self.variance = self.sum_squares / (self.count - 1) as f64;
            self.std_dev = self.variance.sqrt();
        }
    }

    /// Расчёт перцентиля (требует сохранения истории)
    pub fn percentile(&self, p: f64, sorted_values: &[f64]) -> f64 {
        if sorted_values.is_empty() {
            return 0.0;
        }

        let idx = (sorted_values.len() as f64 * p / 100.0) as usize;
        sorted_values[idx.min(sorted_values.len() - 1)]
    }
}

#[derive(Debug, Clone)]
pub struct TimeSeriesModel {
    /// Уровень (level)
    pub level: f64,

    /// Тренд (trend)
    pub trend: f64,

    /// Сезонные компоненты
    pub seasonal: Vec<f64>,

    /// Длина сезона
    pub season_length: usize,

    /// Коэффициент сглаживания уровня (α)
    pub alpha: f64,

    /// Коэффициент сглаживания тренда (β)
    pub beta: f64,

    /// Коэффициент сглаживания сезонности (γ)
    pub gamma: f64,

    /// Прогноз на следующий шаг
    pub forecast: f64,

    /// Ошибка прогноза
    pub forecast_error: f64,

    /// Доверительный интервал
    pub confidence_interval: f64,
}

impl TimeSeriesModel {
    pub fn new(season_length: usize) -> Self {
        Self {
            level: 0.0,
            trend: 0.0,
            seasonal: vec![0.0; season_length],
            season_length,
            alpha: 0.3,
            beta: 0.1,
            gamma: 0.2,
            forecast: 0.0,
            forecast_error: 0.0,
            confidence_interval: 0.0,
        }
    }

    /// Обновление модели Хольта-Винтерса
    pub fn update(&mut self, observation: f64, t: usize) -> f64 {
        let season_idx = t % self.season_length;

        // Предыдущий уровень
        let last_level = self.level;

        // Прогноз
        self.forecast = self.level + self.trend + self.seasonal[season_idx];

        // Ошибка прогноза
        self.forecast_error = observation - self.forecast;

        // Обновление уровня
        self.level = self.alpha * (observation - self.seasonal[season_idx])
            + (1.0 - self.alpha) * (self.level + self.trend);

        // Обновление тренда
        self.trend = self.beta * (self.level - last_level)
            + (1.0 - self.beta) * self.trend;

        // Обновление сезонности
        self.seasonal[season_idx] = self.gamma * (observation - last_level - self.trend)
            + (1.0 - self.gamma) * self.seasonal[season_idx];

        // Расчёт доверительного интервала (95%)
        self.confidence_interval = 1.96 * self.forecast_error.abs();

        self.forecast
    }
}

#[derive(Debug, Clone)]
pub struct AggregatedMetric {
    pub name: String,
    pub distribution: MetricDistributionModel,
    pub time_series: TimeSeriesModel,
    pub values: VecDeque<f64>,
    pub timestamps: VecDeque<Instant>,
    pub max_history: usize,
    pub last_updated: Option<Instant>,
    pub labels: HashMap<String, String>,
}

impl AggregatedMetric {
    pub fn new(name: String, max_history: usize) -> Self {
        Self {
            name,
            distribution: MetricDistributionModel::new(),
            time_series: TimeSeriesModel::new(24), // Суточная сезонность
            values: VecDeque::with_capacity(max_history),
            timestamps: VecDeque::with_capacity(max_history),
            max_history,
            last_updated: None,
            labels: HashMap::new(),
        }
    }

    /// Добавление значения
    pub fn add_value(&mut self, value: f64) {
        let now = Instant::now();

        // Обновление распределения
        self.distribution.update(value);

        // Обновление временного ряда
        self.time_series.update(value, self.values.len());

        // Сохранение истории
        self.values.push_back(value);
        self.timestamps.push_back(now);

        if self.values.len() > self.max_history {
            self.values.pop_front();
            self.timestamps.pop_front();
        }

        self.last_updated = Some(now);
    }

    /// Получение отсортированных значений
    pub fn sorted_values(&self) -> Vec<f64> {
        let mut sorted: Vec<f64> = self.values.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        sorted
    }

    /// Получение перцентиля
    pub fn percentile(&self, p: f64) -> f64 {
        let sorted = self.sorted_values();
        self.distribution.percentile(p, &sorted)
    }

    /// Получение скорости изменения (производной)
    pub fn derivative(&self) -> f64 {
        if self.values.len() < 2 {
            return 0.0;
        }

        let last = *self.values.back().unwrap_or(&0.0);
        let prev = *self.values.get(self.values.len() - 2).unwrap_or(&0.0);

        last - prev
    }

    /// Получение ускорения (второй производной)
    pub fn second_derivative(&self) -> f64 {
        if self.values.len() < 3 {
            return 0.0;
        }

        let last = *self.values.back().unwrap_or(&0.0);
        let prev = *self.values.get(self.values.len() - 2).unwrap_or(&0.0);
        let prev2 = *self.values.get(self.values.len() - 3).unwrap_or(&0.0);

        last - 2.0 * prev + prev2
    }
}

#[derive(Debug, Clone)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub collection_interval: Duration,
    pub trace_sampling_rate: f64,
    pub service_name: String,
    pub service_version: String,
    pub environment: String,
    pub retention_period: Duration,
    pub max_metrics_per_service: usize,
    pub enable_histograms: bool,
    pub enable_time_series: bool,
    pub enable_anomaly_detection: bool,
    pub anomaly_threshold: f64,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            collection_interval: Duration::from_secs(5),
            trace_sampling_rate: 0.1,
            service_name: "batch-system".to_string(),
            service_version: "2.0.0".to_string(),
            environment: "production".to_string(),
            retention_period: Duration::from_secs(3600),
            max_metrics_per_service: 1000,
            enable_histograms: true,
            enable_time_series: true,
            enable_anomaly_detection: true,
            anomaly_threshold: 3.0, // 3 сигмы
        }
    }
}

#[derive(Debug, Clone)]
pub struct Anomaly {
    pub metric_name: String,
    pub timestamp: Instant,
    pub value: f64,
    pub expected_value: f64,
    pub deviation: f64,
    pub threshold: f64,
    pub severity: AnomalySeverity,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnomalySeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("Tracing not initialized")]
    TracingNotInitialized,

    #[error("Metric not found: {0}")]
    MetricNotFound(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Storage error: {0}")]
    StorageError(String),
}

pub struct MetricsTracingSystem {
    metrics_store: Arc<DashMap<String, AggregatedMetric>>,
    system_health: Arc<RwLock<SystemHealthModel>>,
    anomaly_detector: Arc<RwLock<AnomalyDetector>>,
    config: MetricsConfig,
    total_metrics_recorded: Arc<AtomicU64>,
    total_anomalies_detected: Arc<AtomicU64>,
    start_time: Instant,
    anomalies: Arc<RwLock<VecDeque<Anomaly>>>,
    recent_anomalies: Arc<DashMap<String, Instant>>,
    anomaly_cooldown: Duration,
}

#[derive(Debug, Clone)]
pub struct SystemHealthModel {
    pub overall_health: f64,
    pub component_health: HashMap<String, f64>,
    pub latency_health: f64,
    pub throughput_health: f64,
    pub error_rate_health: f64,
    pub memory_health: f64,
    pub cpu_health: f64,
    pub last_updated: Instant,
}

impl SystemHealthModel {
    pub fn new() -> Self {
        Self {
            overall_health: 1.0,
            component_health: HashMap::new(),
            latency_health: 1.0,
            throughput_health: 1.0,
            error_rate_health: 1.0,
            memory_health: 1.0,
            cpu_health: 1.0,
            last_updated: Instant::now(),
        }
    }

    /// Обновление здоровья на основе метрик
    pub fn update(&mut self, metrics: &DashMap<String, AggregatedMetric>) {
        // Здоровье по задержке
        if let Some(metric) = metrics.get("latency.p95") {
            let p95 = metric.percentile(95.0);
            self.latency_health = 1.0 / (1.0 + (p95 / 100.0).min(10.0));
        }

        // Здоровье по пропускной способности
        if let Some(metric) = metrics.get("throughput") {
            let throughput = metric.distribution.mean;
            self.throughput_health = (throughput / 100000.0).min(1.0);
        }

        // Здоровье по ошибкам
        if let Some(metric) = metrics.get("system.errors") {
            let error_rate = metric.derivative();
            self.error_rate_health = 1.0 / (1.0 + error_rate * 100.0);
        }

        // Общее здоровье (среднее взвешенное)
        self.overall_health =
            self.latency_health * 0.4 +
            self.throughput_health * 0.3 +
            self.error_rate_health * 0.2 +
            self.memory_health * 0.05 +
            self.cpu_health * 0.05;

        self.last_updated = Instant::now();
    }
}

#[derive(Debug, Clone)]
pub struct AnomalyDetector {
    /// Порог для Z-score
    pub zscore_threshold: f64,

    /// Порог для MAD (Median Absolute Deviation)
    pub mad_threshold: f64,

    /// Порог для изменения тренда
    pub trend_threshold: f64,

    /// История аномалий
    pub history: VecDeque<Anomaly>,
    trend_history: DashMap<String, VecDeque<f64>>,
    min_samples_for_trend: usize,
}

impl AnomalyDetector {
    pub fn new() -> Self {
        Self {
            zscore_threshold: 3.0,
            mad_threshold: 3.5,
            trend_threshold: 0.5,
            history: VecDeque::with_capacity(1000),
            trend_history: DashMap::new(),
            min_samples_for_trend: 5,
        }
    }

    /// Детекция аномалий методом Z-score
    pub fn detect_zscore(&self, value: f64, mean: f64, std_dev: f64) -> Option<f64> {
        if std_dev <= 0.0 {
            return None;
        }

        let zscore = (value - mean).abs() / std_dev;

        if zscore > self.zscore_threshold {
            Some(zscore)
        } else {
            None
        }
    }

    /// Детекция аномалий методом MAD
    pub fn detect_mad(&self, value: f64, median: f64, mad: f64) -> Option<f64> {
        if mad <= 0.0 {
            return None;
        }

        let modified_zscore = 0.6745 * (value - median).abs() / mad;

        if modified_zscore > self.mad_threshold {
            Some(modified_zscore)
        } else {
            None
        }
    }

    /// Детекция аномалий по изменению тренда
    pub fn detect_trend_change(&self, metric_name: &str, current_trend: f64) -> bool {
        let mut history = self.trend_history
            .entry(metric_name.to_string())
            .or_insert_with(|| VecDeque::with_capacity(10));

        history.push_back(current_trend);
        if history.len() > 10 {
            history.pop_front();
        }

        // Нужно минимум 5 измерений
        if history.len() < 5 {
            return false;
        }

        // БЕРЁМ ПРЕДЫДУЩЕЕ ЗНАЧЕНИЕ, НЕ ТЕКУЩЕЕ!
        let prev_trend = history[history.len() - 2];

        // ИСПРАВЛЕНО: Если тренд не изменился - НЕ АНОМАЛИЯ
        if (current_trend - prev_trend).abs() < 0.1 {
            return false;
        }

        // ИСПРАВЛЕНО: Игнорируем тренды около нуля при отсутствии нагрузки
        if history.iter().all(|&t| t.abs() < 0.1) {
            return false;
        }

        // Проверяем резкое изменение
        let change = (current_trend - prev_trend).abs();
        change > self.trend_threshold
    }

    /// Расчёт MAD (Median Absolute Deviation)
    pub fn calculate_mad(values: &[f64], median: f64) -> f64 {
        let deviations: Vec<f64> = values.iter()
            .map(|&x| (x - median).abs())
            .collect();

        let mut sorted = deviations.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let mid = sorted.len() / 2;
        if sorted.len() % 2 == 0 {
            (sorted[mid - 1] + sorted[mid]) / 2.0
        } else {
            sorted[mid]
        }
    }
}

impl MetricsTracingSystem {
    pub fn new(config: MetricsConfig) -> Result<Self, MetricsError> {
        info!("📊 Initializing Mathematical Metrics & Tracing System v2.0");
        info!("  Service: {} v{}", config.service_name, config.service_version);
        info!("  Environment: {}", config.environment);
        info!("  Collection interval: {:?}", config.collection_interval);
        info!("  Sampling rate: {:.1}%", config.trace_sampling_rate * 100.0);
        info!("  Histograms: {}", if config.enable_histograms { "enabled" } else { "disabled" });
        info!("  Time series: {}", if config.enable_time_series { "enabled" } else { "disabled" });
        info!("  Anomaly detection: {}", if config.enable_anomaly_detection { "enabled" } else { "disabled" });

        if config.enabled && !tracing::dispatcher::has_been_set() {
            return Err(MetricsError::TracingNotInitialized);
        }

        Ok(Self {
            metrics_store: Arc::new(DashMap::with_capacity(1000)),
            system_health: Arc::new(RwLock::new(SystemHealthModel::new())),
            anomaly_detector: Arc::new(RwLock::new(AnomalyDetector::new())),
            config,
            total_metrics_recorded: Arc::new(AtomicU64::new(0)),
            total_anomalies_detected: Arc::new(AtomicU64::new(0)),
            start_time: Instant::now(),
            anomalies: Arc::new(RwLock::new(VecDeque::with_capacity(1000))),
            recent_anomalies: Arc::new(DashMap::new()),
            anomaly_cooldown: Duration::from_secs(300), // 5 минут
        })
    }

    pub async fn record_metric(&self, name: &str, value: f64) {
        if !self.config.enabled {
            return;
        }

        let key = name.to_string();

        // Получение или создание метрики
        let mut metric = self.metrics_store
            .entry(key.clone())
            .or_insert_with(|| AggregatedMetric::new(key, 1000));

        // Добавление значения
        metric.add_value(value);

        // Обновление счётчика
        self.total_metrics_recorded.fetch_add(1, Ordering::Relaxed);

        // Детекция аномалий
        if self.config.enable_anomaly_detection {
            self.detect_anomalies(&metric, value).await;
        }
    }

    pub fn record_metric_with_labels(&self, name: &str, value: f64, labels: HashMap<String, String>) {
        if !self.config.enabled {
            return;
        }

        let key = name.to_string();

        let mut metric = self.metrics_store
            .entry(key.clone())
            .or_insert_with(|| AggregatedMetric::new(key, 1000));

        metric.labels = labels;
        metric.add_value(value);

        self.total_metrics_recorded.fetch_add(1, Ordering::Relaxed);
    }

    async fn detect_anomalies(&self, metric: &AggregatedMetric, value: f64) {
        if metric.distribution.count < 10 {
            return;
        }

        if metric.distribution.std_dev < 0.001 {
            return;
        }

        let detector = self.anomaly_detector.read().await;
        let dist = &metric.distribution;

        // Z-score детекция
        if let Some(zscore) = detector.detect_zscore(value, dist.mean, dist.std_dev) {
            let severity = if zscore > 5.0 {
                AnomalySeverity::Critical
            } else if zscore > 4.0 {
                AnomalySeverity::Warning
            } else {
                AnomalySeverity::Info
            };

            let anomaly = Anomaly {
                metric_name: metric.name.clone(),
                timestamp: Instant::now(),
                value,
                expected_value: dist.mean,
                deviation: zscore,
                threshold: detector.zscore_threshold,
                severity,
                description: format!("Z-score anomaly: {:.2}σ", zscore),
            };

            self.report_anomaly(anomaly);
        }

        // MAD детекция
        let sorted = metric.sorted_values();
        if !sorted.is_empty() {
            let median = sorted[sorted.len() / 2];
            let mad = AnomalyDetector::calculate_mad(&sorted, median);

            if let Some(mad_score) = detector.detect_mad(value, median, mad) {
                let anomaly = Anomaly {
                    metric_name: metric.name.clone(),
                    timestamp: Instant::now(),
                    value,
                    expected_value: median,
                    deviation: mad_score,
                    threshold: detector.mad_threshold,
                    severity: AnomalySeverity::Warning,
                    description: format!("MAD anomaly: {:.2}", mad_score),
                };

                self.report_anomaly(anomaly);
            }
        }

        // Тренд-детекция
        if metric.values.len() >= 10 {
            let recent_trend = metric.derivative();

            if detector.detect_trend_change(&metric.name, recent_trend) {
                let anomaly = Anomaly {
                    metric_name: metric.name.clone(),
                    timestamp: Instant::now(),
                    value: recent_trend,
                    expected_value: recent_trend,
                    deviation: (recent_trend - recent_trend).abs(),
                    threshold: detector.trend_threshold,
                    severity: AnomalySeverity::Warning,
                    description: format!("Trend change: {:.2} → {:.2}", recent_trend, recent_trend),
                };

                self.report_anomaly(anomaly);
            }
        }
    }

    fn report_anomaly(&self, anomaly: Anomaly) {
        // ИСПРАВЛЕНО: Более уникальный ключ
        let key = format!("{}:{}:{:.2}",
                          anomaly.metric_name,
                          anomaly.description,
                          anomaly.timestamp.elapsed().as_secs() / 60  // меняется каждую минуту
        );

        // Проверяем, не было ли такой аномалии в последние 5 минут
        if let Some(last_time) = self.recent_anomalies.get(&key) {
            if last_time.elapsed() < self.anomaly_cooldown {
                debug!("⏸️ Anomaly suppressed (cooldown): {}", key);
                return;
            }
        }

        // ИСПРАВЛЕНО: Очищаем старые записи ПЕРЕД вставкой
        self.recent_anomalies.retain(|_, time| {
            time.elapsed() < Duration::from_secs(3600)
        });

        self.recent_anomalies.insert(key, Instant::now());

        // ИСПРАВЛЕНО: Логируем ТОЛЬКО ЗДЕСЬ, убираем лог из integration.rs
        match anomaly.severity {
            AnomalySeverity::Info => debug!("ℹ️ {}: {}", anomaly.metric_name, anomaly.description),
            AnomalySeverity::Warning => warn!("⚠️ {}: {}", anomaly.metric_name, anomaly.description),
            AnomalySeverity::Critical => error!("🚨 {}: {}", anomaly.metric_name, anomaly.description),
        }

        self.total_anomalies_detected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get_aggregated_metric(&self, name: &str) -> Option<AggregatedMetric> {
        self.metrics_store.get(name).map(|m| m.clone())  // ← ЭТО ДОЛЖНО РАБОТАТЬ!
    }

    pub fn get_all_metrics(&self) -> Vec<AggregatedMetric> {
        self.metrics_store
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    pub async fn get_system_health(&self) -> f64 {
        let mut health = self.system_health.write().await;
        health.update(&self.metrics_store);
        health.overall_health
    }

    pub async fn get_anomalies(&self, limit: usize) -> Vec<Anomaly> {
        let anomalies = self.anomalies.read().await;
        let start = if anomalies.len() > limit {
            anomalies.len() - limit
        } else {
            0
        };
        anomalies.iter().skip(start).cloned().collect()
    }

    pub fn get_stats(&self) -> MetricsStats {
        MetricsStats {
            total_metrics: self.metrics_store.len() as u64,
            total_recorded: self.total_metrics_recorded.load(Ordering::Relaxed),
            total_anomalies: self.total_anomalies_detected.load(Ordering::Relaxed),
            uptime: self.start_time.elapsed(),
            collection_interval: self.config.collection_interval,
            sampling_rate: self.config.trace_sampling_rate,
        }
    }

    pub fn reset_metrics(&self) {
        self.metrics_store.clear();
        self.total_metrics_recorded.store(0, Ordering::Relaxed);
        self.total_anomalies_detected.store(0, Ordering::Relaxed);

        if let Ok(mut anomalies) = self.anomalies.try_write() {
            anomalies.clear();
        }

        info!("📊 All metrics reset");
    }
}

#[derive(Debug, Clone)]
pub struct MetricsStats {
    pub total_metrics: u64,
    pub total_recorded: u64,
    pub total_anomalies: u64,
    pub uptime: Duration,
    pub collection_interval: Duration,
    pub sampling_rate: f64,
}

impl Clone for MetricsTracingSystem {
    fn clone(&self) -> Self {
        Self {
            metrics_store: self.metrics_store.clone(),
            system_health: self.system_health.clone(),
            anomaly_detector: self.anomaly_detector.clone(),
            config: self.config.clone(),
            total_metrics_recorded: self.total_metrics_recorded.clone(),
            total_anomalies_detected: self.total_anomalies_detected.clone(),
            start_time: self.start_time,
            anomalies: self.anomalies.clone(),
            anomaly_cooldown: self.anomaly_cooldown.clone(),
            recent_anomalies: self.recent_anomalies.clone(),
        }
    }
}