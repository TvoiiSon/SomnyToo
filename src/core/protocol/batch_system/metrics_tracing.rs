use std::sync::Arc;
use std::time::{Instant, Duration};
use tokio::sync::RwLock;
use tracing::{info, error};
use dashmap::DashMap;

/// Упрощенная система метрик и трассировки
pub struct MetricsTracingSystem {
    config: MetricsConfig,
    metrics_store: Arc<DashMap<String, MetricSeries>>,
    trace_sampler: Arc<RwLock<TraceSampler>>,
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
}

#[derive(Debug, Clone)]
pub struct MetricSeries {
    name: String,
    values: Vec<(Instant, f64)>,
    aggregation: AggregationType,
    max_samples: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AggregationType {
    Average,
    Sum,
    Max,
    Min,
    Count,
}

#[derive(Debug, Clone)]
struct TraceSampler {
    sampling_rate: f64,
    sampled_traces: u64,
    total_traces: u64,
}

impl MetricsTracingSystem {
    pub fn new(config: MetricsConfig) -> Result<Self, MetricsError> {
        info!("📊 Инициализация системы метрик и трассировки");

        if config.enabled {
            // НЕ инициализируем tracing здесь - он уже инициализирован в основном приложении
            // Вместо этого проверяем, что tracing работает
            if !tracing::dispatcher::has_been_set() {
                return Err(MetricsError::InitializationError(
                    "Tracing не инициализирован. Нужно вызвать init_tracing() в основном приложении".to_string()
                ));
            }

            Ok(Self {
                config: config.clone(),
                metrics_store: Arc::new(DashMap::new()),
                trace_sampler: Arc::new(RwLock::new(TraceSampler {
                    sampling_rate: config.trace_sampling_rate,
                    sampled_traces: 0,
                    total_traces: 0,
                })),
            })
        } else {
            info!("📊 Система метрик отключена");
            Ok(Self {
                config,
                metrics_store: Arc::new(DashMap::new()),
                trace_sampler: Arc::new(RwLock::new(TraceSampler {
                    sampling_rate: 0.0,
                    sampled_traces: 0,
                    total_traces: 0,
                })),
            })
        }
    }

    /// Инициализация tracing (должна быть вызвана только один раз в приложении)
    pub fn init_tracing() -> Result<(), MetricsError> {
        // Проверяем, не инициализирован ли tracing уже
        if tracing::dispatcher::has_been_set() {
            info!("📊 Tracing уже инициализирован, пропускаем повторную инициализацию");
            return Ok(()); // Уже инициализирован
        }

        // Альтернативный, более простой способ инициализации tracing
        let subscriber = tracing_subscriber::FmtSubscriber::builder()
            .with_target(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .finish();

        tracing::subscriber::set_global_default(subscriber)
            .map_err(|e| MetricsError::InitializationError(e.to_string()))?;

        info!("📊 Tracing успешно инициализирован");
        Ok(())
    }

    /// Получение агрегированных метрик
    pub fn get_aggregated_metrics(&self, name: &str) -> Option<AggregatedMetric> {
        let series = self.metrics_store.get(name)?;

        let values: Vec<f64> = series.values.iter()
            .map(|(_, v)| *v)
            .collect();

        if values.is_empty() {
            return None;
        }

        let count = values.len();
        let sum: f64 = values.iter().sum();
        let avg = sum / count as f64;
        let min = values.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let max = values.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));

        // Рассчитываем процентили
        let mut sorted = values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let p50 = Self::percentile(&sorted, 0.5);
        let p95 = Self::percentile(&sorted, 0.95);
        let p99 = Self::percentile(&sorted, 0.99);

        Some(AggregatedMetric {
            name: name.to_string(),
            count,
            sum,
            avg,
            min,
            max,
            p50,
            p95,
            p99,
            last_updated: series.values.last().map(|(t, _)| *t),
        })
    }

    fn percentile(sorted: &[f64], percentile: f64) -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }

        let index = (sorted.len() as f64 * percentile).ceil() as usize - 1;
        sorted[index.min(sorted.len() - 1)]
    }
}

#[derive(Debug, Clone)]
pub struct AggregatedMetric {
    pub name: String,
    pub count: usize,
    pub sum: f64,
    pub avg: f64,
    pub min: f64,
    pub max: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub last_updated: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct ExportedMetric {
    pub name: String,
    pub aggregation_type: AggregationType,
    pub aggregated: AggregatedMetric,
}

#[derive(Debug, Clone)]
pub struct TraceStats {
    pub sampling_rate: f64,
    pub total_traces: u64,
    pub sampled_traces: u64,
    pub sampling_efficiency: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("Ошибка инициализации: {0}")]
    InitializationError(String),
}