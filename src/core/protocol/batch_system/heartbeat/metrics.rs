use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct BatchSystemMetric {
    pub name: String,
    pub value: MetricValue,
    pub timestamp: SystemTime,
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub enum MetricValue {
    Counter(u64),
    Gauge(f64),
    Histogram(Vec<f64>),
    Summary {
        count: u64,
        sum: f64,
        quantiles: Vec<(f64, f64)>, // Changed from HashMap<f64, f64> to Vec<(f64, f64)>
    },
}

#[derive(Clone)]
pub struct BatchSystemMetricsCollector {
    metrics: Arc<RwLock<HashMap<String, Vec<BatchSystemMetric>>>>,
    retention_period: Duration,
    max_metrics_per_name: usize,
}

impl BatchSystemMetricsCollector {
    pub fn new(retention_period: Duration, max_metrics_per_name: usize) -> Self {
        Self {
            metrics: Arc::new(RwLock::new(HashMap::new())),
            retention_period,
            max_metrics_per_name,
        }
    }

    pub async fn record_metric(&self, name: &str, value: MetricValue, tags: Option<HashMap<String, String>>) {
        let metric = BatchSystemMetric {
            name: name.to_string(),
            value,
            timestamp: SystemTime::now(),
            tags: tags.unwrap_or_default(),
        };

        let mut metrics = self.metrics.write().await;
        let entry = metrics.entry(name.to_string()).or_insert_with(Vec::new);
        entry.push(metric);

        // Очищаем старые метрики
        self.cleanup_old_metrics(name).await;
    }

    async fn cleanup_old_metrics(&self, name: &str) {
        let mut metrics = self.metrics.write().await;
        if let Some(metric_list) = metrics.get_mut(name) {
            let cutoff_time = SystemTime::now() - self.retention_period;

            // Удаляем метрики старше retention_period
            metric_list.retain(|m| {
                m.timestamp > cutoff_time
            });

            // Ограничиваем количество метрик
            if metric_list.len() > self.max_metrics_per_name {
                let excess = metric_list.len() - self.max_metrics_per_name;
                metric_list.drain(0..excess);
            }
        }
    }

    pub async fn get_metric_history(&self, name: &str, limit: Option<usize>) -> Vec<BatchSystemMetric> {
        let metrics = self.metrics.read().await;
        if let Some(metric_list) = metrics.get(name) {
            let start = if let Some(limit) = limit {
                metric_list.len().saturating_sub(limit)
            } else {
                0
            };
            metric_list[start..].to_vec()
        } else {
            Vec::new()
        }
    }

    pub async fn get_metric_names(&self) -> Vec<String> {
        let metrics = self.metrics.read().await;
        metrics.keys().cloned().collect()
    }

    pub async fn calculate_statistics(&self, name: &str) -> Option<MetricStatistics> {
        let metrics = self.get_metric_history(name, None).await;

        if metrics.is_empty() {
            return None;
        }

        let numeric_values: Vec<f64> = metrics.iter()
            .filter_map(|m| match &m.value {
                MetricValue::Gauge(v) => Some(*v),
                MetricValue::Counter(v) => Some(*v as f64),
                _ => None,
            })
            .collect();

        if numeric_values.is_empty() {
            return None;
        }

        let count = numeric_values.len();
        let sum: f64 = numeric_values.iter().sum();
        let avg = sum / count as f64;

        let variance: f64 = numeric_values.iter()
            .map(|v| (v - avg).powi(2))
            .sum::<f64>() / count as f64;
        let std_dev = variance.sqrt();

        let mut sorted = numeric_values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let p50 = sorted[(count as f64 * 0.5) as usize];
        let p95 = sorted[(count as f64 * 0.95) as usize];
        let p99 = sorted[(count as f64 * 0.99) as usize];

        Some(MetricStatistics {
            count,
            sum,
            avg,
            min: *sorted.first().unwrap(),
            max: *sorted.last().unwrap(),
            std_dev,
            p50,
            p95,
            p99,
        })
    }

    pub async fn reset_metrics(&self) {
        let mut metrics = self.metrics.write().await;
        metrics.clear();
    }

    pub async fn export_metrics(&self) -> HashMap<String, Vec<BatchSystemMetric>> {
        let metrics = self.metrics.read().await;
        metrics.clone()
    }
}

#[derive(Debug, Clone)]
pub struct MetricStatistics {
    pub count: usize,
    pub sum: f64,
    pub avg: f64,
    pub min: f64,
    pub max: f64,
    pub std_dev: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}