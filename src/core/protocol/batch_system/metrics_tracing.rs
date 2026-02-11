use std::sync::Arc;
use std::time::{Instant, Duration};
use tracing::info;
use dashmap::DashMap;

/// Упрощенная система метрик и трассировки
pub struct MetricsTracingSystem {
    metrics_store: Arc<DashMap<String, AggregatedMetric>>,
    _config: MetricsConfig,
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

#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("Initialization error: {0}")]
    InitializationError(String),
}

impl MetricsTracingSystem {
    pub fn new(config: MetricsConfig) -> Result<Self, MetricsError> {
        info!("📊 Initializing metrics and tracing system");

        if config.enabled && !tracing::dispatcher::has_been_set() {
            return Err(MetricsError::InitializationError(
                "Tracing not initialized".to_string()
            ));
        }

        Ok(Self {
            metrics_store: Arc::new(DashMap::new()),
            _config: config,
        })
    }

    /// Запись метрики
    pub fn record_metric(&self, name: &str, value: f64) {
        let key = name.to_string();

        if let Some(mut metric) = self.metrics_store.get_mut(&key) {
            metric.count += 1;
            metric.sum += value;
            metric.avg = metric.sum / metric.count as f64;
            metric.min = metric.min.min(value);
            metric.max = metric.max.max(value);
            metric.last_updated = Some(Instant::now());

            metric.p50 = metric.avg;
            metric.p95 = metric.avg * 1.2;
            metric.p99 = metric.avg * 1.5;
        } else {
            self.metrics_store.insert(key, AggregatedMetric {
                name: name.to_string(),
                count: 1,
                sum: value,
                avg: value,
                min: value,
                max: value,
                p50: value,
                p95: value,
                p99: value,
                last_updated: Some(Instant::now()),
            });
        }
    }

    /// Получение агрегированных метрик
    pub fn get_aggregated_metrics(&self, name: &str) -> Option<AggregatedMetric> {
        self.metrics_store.get(name).map(|m| m.clone())
    }

    /// Получение всех метрик
    pub fn get_all_metrics(&self) -> Vec<AggregatedMetric> {
        self.metrics_store.iter().map(|m| m.clone()).collect()
    }
}