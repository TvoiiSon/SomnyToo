use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::super::unified_monitor::{Monitor, MonitorMetrics, MetricValue, UnifiedMonitor, AlertLevel};

#[derive(Debug, Clone)]
pub struct BatchSystemMetrics {
    pub buffer_pool_hit_rate: f64,
    pub buffer_pool_reuse_rate: f64,
    pub crypto_success_rate: f64,
    pub work_stealing_tasks: u64,
    pub active_connections: u64,
    pub pending_batches: u64,
    pub avg_processing_time_ms: f64,
    pub batch_completion_rate: f64,
}

#[derive(Clone)]
pub struct BatchSystemMonitor {
    metrics: Arc<RwLock<BatchSystemMetrics>>,
    unified_monitor: Option<Arc<UnifiedMonitor>>,
}

impl BatchSystemMonitor {
    pub fn new() -> Self {
        Self {
            metrics: Arc::new(RwLock::new(BatchSystemMetrics {
                buffer_pool_hit_rate: 0.0,
                buffer_pool_reuse_rate: 0.0,
                crypto_success_rate: 1.0,
                work_stealing_tasks: 0,
                active_connections: 0,
                pending_batches: 0,
                avg_processing_time_ms: 0.0,
                batch_completion_rate: 0.0,
            })),
            unified_monitor: None,
        }
    }

    pub fn attach_to_unified(&mut self, unified: Arc<UnifiedMonitor>) {
        self.unified_monitor = Some(unified);
    }

    pub async fn record_buffer_pool_hit(&self, hit_rate: f64, reuse_rate: f64) {
        let mut metrics = self.metrics.write().await;
        metrics.buffer_pool_hit_rate = hit_rate;
        metrics.buffer_pool_reuse_rate = reuse_rate;

        if hit_rate < 0.3 && self.unified_monitor.is_some() {
            let unified = self.unified_monitor.as_ref().unwrap();
            unified.add_alert(
                AlertLevel::Warning,
                "batch_system",
                &format!("Low buffer pool hit rate: {:.1}%", hit_rate * 100.0)
            ).await;
        }
    }

    pub async fn record_crypto_success(&self, success_rate: f64) {
        let mut metrics = self.metrics.write().await;
        metrics.crypto_success_rate = success_rate;

        if success_rate < 0.95 && self.unified_monitor.is_some() {
            let unified = self.unified_monitor.as_ref().unwrap();
            unified.add_alert(
                AlertLevel::Error,
                "batch_system",
                &format!("Crypto processor success rate low: {:.1}%", success_rate * 100.0)
            ).await;
        }
    }

    pub async fn record_work_stealing_tasks(&self, task_count: u64) {
        let mut metrics = self.metrics.write().await;
        metrics.work_stealing_tasks = task_count;

        if task_count == 0 && self.unified_monitor.is_some() {
            let unified = self.unified_monitor.as_ref().unwrap();
            unified.add_alert(
                AlertLevel::Warning,
                "batch_system",
                "No active work-stealing tasks"
            ).await;
        }
    }

    pub async fn record_connection_count(&self, connection_count: u64) {
        let mut metrics = self.metrics.write().await;
        metrics.active_connections = connection_count;

        if connection_count == 0 && self.unified_monitor.is_some() {
            let unified = self.unified_monitor.as_ref().unwrap();
            unified.add_alert(
                AlertLevel::Info,
                "batch_system",
                "No active connections"
            ).await;
        }
    }

    pub async fn record_pending_batches(&self, batch_count: u64) {
        let mut metrics = self.metrics.write().await;
        metrics.pending_batches = batch_count;
    }

    pub async fn record_processing_time(&self, processing_time_ms: f64) {
        let mut metrics = self.metrics.write().await;
        metrics.avg_processing_time_ms = processing_time_ms;

        if processing_time_ms > 1000.0 && self.unified_monitor.is_some() {
            let unified = self.unified_monitor.as_ref().unwrap();
            unified.add_alert(
                AlertLevel::Warning,
                "batch_system",
                &format!("High processing time: {:.1}ms", processing_time_ms)
            ).await;
        }
    }

    pub async fn record_batch_completion(&self, completion_rate: f64) {
        let mut metrics = self.metrics.write().await;
        metrics.batch_completion_rate = completion_rate;

        if completion_rate < 0.9 && self.unified_monitor.is_some() {
            let unified = self.unified_monitor.as_ref().unwrap();
            unified.add_alert(
                AlertLevel::Warning,
                "batch_system",
                &format!("Low batch completion rate: {:.1}%", completion_rate * 100.0)
            ).await;
        }
    }

    pub async fn get_current_metrics(&self) -> BatchSystemMetrics {
        let metrics = self.metrics.read().await;
        metrics.clone()
    }
}

#[async_trait::async_trait]
impl Monitor for BatchSystemMonitor {
    fn name(&self) -> &'static str {
        "batch_system"
    }

    async fn collect_metrics(&self) -> MonitorMetrics {
        let metrics = self.metrics.read().await;
        let mut metric_map = HashMap::new();

        metric_map.insert("buffer_pool_hit_rate".to_string(),
                          MetricValue::Gauge(metrics.buffer_pool_hit_rate));
        metric_map.insert("buffer_pool_reuse_rate".to_string(),
                          MetricValue::Gauge(metrics.buffer_pool_reuse_rate));
        metric_map.insert("crypto_success_rate".to_string(),
                          MetricValue::Gauge(metrics.crypto_success_rate));
        metric_map.insert("work_stealing_tasks".to_string(),
                          MetricValue::Gauge(metrics.work_stealing_tasks as f64));
        metric_map.insert("active_connections".to_string(),
                          MetricValue::Gauge(metrics.active_connections as f64));
        metric_map.insert("pending_batches".to_string(),
                          MetricValue::Gauge(metrics.pending_batches as f64));
        metric_map.insert("avg_processing_time_ms".to_string(),
                          MetricValue::Gauge(metrics.avg_processing_time_ms));
        metric_map.insert("batch_completion_rate".to_string(),
                          MetricValue::Gauge(metrics.batch_completion_rate));

        MonitorMetrics {
            name: self.name().to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            metrics: metric_map,
            health: self.health_check().await,
        }
    }

    async fn health_check(&self) -> bool {
        let metrics = self.metrics.read().await;

        // Основные критерии здоровья batch системы:
        // 1. Crypto success rate должен быть выше 95%
        // 2. Buffer pool hit rate должен быть выше 20%
        // 3. Batch completion rate должен быть выше 80%

        let crypto_healthy = metrics.crypto_success_rate > 0.95;
        let buffer_healthy = metrics.buffer_pool_hit_rate > 0.2;
        let batch_healthy = metrics.batch_completion_rate > 0.8;

        crypto_healthy && buffer_healthy && batch_healthy
    }

    async fn reset_metrics(&self) {
        let mut metrics = self.metrics.write().await;
        *metrics = BatchSystemMetrics {
            buffer_pool_hit_rate: metrics.buffer_pool_hit_rate,
            buffer_pool_reuse_rate: metrics.buffer_pool_reuse_rate,
            crypto_success_rate: 1.0,
            work_stealing_tasks: 0,
            active_connections: metrics.active_connections,
            pending_batches: 0,
            avg_processing_time_ms: 0.0,
            batch_completion_rate: 0.0,
        };
    }
}