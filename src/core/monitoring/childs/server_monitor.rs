use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::super::unified_monitor::{Monitor, MonitorMetrics, MetricValue, UnifiedMonitor, AlertLevel};

#[derive(Debug, Clone)]
pub struct ServerMetrics {
    pub active_sessions: u64,
    pub total_connections: u64,
    pub failed_connections: u64,
    pub heartbeat_timeouts: u64,
    pub avg_response_time: f64,
}

#[derive(Clone)]
pub struct ServerMonitor {
    metrics: Arc<RwLock<ServerMetrics>>,
    unified_monitor: Option<Arc<UnifiedMonitor>>,
}

impl ServerMonitor {
    pub fn new() -> Self {
        Self {
            metrics: Arc::new(RwLock::new(ServerMetrics {
                active_sessions: 0,
                total_connections: 0,
                failed_connections: 0,
                heartbeat_timeouts: 0,
                avg_response_time: 0.0,
            })),
            unified_monitor: None,
        }
    }

    pub fn attach_to_unified(&mut self, unified: Arc<UnifiedMonitor>) {
        self.unified_monitor = Some(unified);
    }

    pub async fn record_session_connected(&self) {
        let mut metrics = self.metrics.write().await;
        metrics.active_sessions += 1;
        metrics.total_connections += 1;
    }

    pub async fn record_session_disconnected(&self) {
        let mut metrics = self.metrics.write().await;
        metrics.active_sessions = metrics.active_sessions.saturating_sub(1);
    }

    pub async fn record_heartbeat_timeout(&self) {
        let mut metrics = self.metrics.write().await;
        metrics.heartbeat_timeouts += 1;

        if let Some(ref unified) = self.unified_monitor {
            unified.add_alert(AlertLevel::Warning, "server", "Heartbeat timeout detected").await;
        }
    }

    pub async fn record_failed_connection(&self) {
        let mut metrics = self.metrics.write().await;
        metrics.failed_connections += 1;

        if let Some(ref unified) = self.unified_monitor && metrics.failed_connections % 5 == 0 {
            unified.add_alert(
                AlertLevel::Error,
                "server",
                &format!("Multiple connection failures: {}", metrics.failed_connections)
            ).await;
        }
    }
}

#[async_trait::async_trait]
impl Monitor for ServerMonitor {
    fn name(&self) -> &'static str {
        "server"
    }

    async fn collect_metrics(&self) -> MonitorMetrics {
        let metrics = self.metrics.read().await;
        let mut metric_map = HashMap::new();

        metric_map.insert("active_sessions".to_string(), MetricValue::Gauge(metrics.active_sessions as f64));
        metric_map.insert("total_connections".to_string(), MetricValue::Counter(metrics.total_connections));
        metric_map.insert("failed_connections".to_string(), MetricValue::Counter(metrics.failed_connections));
        metric_map.insert("heartbeat_timeouts".to_string(), MetricValue::Counter(metrics.heartbeat_timeouts));
        metric_map.insert("avg_response_time".to_string(), MetricValue::Gauge(metrics.avg_response_time));

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
        metrics.heartbeat_timeouts < 10
    }

    async fn reset_metrics(&self) {
        let mut metrics = self.metrics.write().await;
        *metrics = ServerMetrics {
            active_sessions: metrics.active_sessions,
            total_connections: metrics.total_connections,
            failed_connections: 0,
            heartbeat_timeouts: 0,
            avg_response_time: 0.0,
        };
    }
}