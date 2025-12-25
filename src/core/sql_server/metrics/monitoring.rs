use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Serialize, Clone)]
pub struct RealTimeMetrics {
    pub timestamp: u64,
    pub queries_per_second: f64,
    pub average_response_time: f64,
    pub active_connections: usize,
    pub cache_hit_rate: f64,
    pub memory_usage_mb: f64,
    pub error_rate: f64,
    pub replica_lag_ms: Option<f64>,
}

pub struct PerformanceMonitor {
    metrics_sender: broadcast::Sender<RealTimeMetrics>,
    last_update: Instant,
    query_count: u64,
    total_response_time: Duration,
    error_count: u64,
    server_metrics: Arc<RwLock<Option<Arc<super::metrics::MetricsCollector>>>>,
}

impl PerformanceMonitor {
    pub fn new() -> (Self, broadcast::Receiver<RealTimeMetrics>) {
        let (sender, receiver) = broadcast::channel(100);

        let monitor = Self {
            metrics_sender: sender,
            last_update: Instant::now(),
            query_count: 0,
            total_response_time: Duration::default(),
            error_count: 0,
            server_metrics: Arc::new(RwLock::new(None)),
        };

        (monitor, receiver)
    }

    pub async fn register_server_metrics(&self, metrics: Arc<super::metrics::MetricsCollector>) {
        let mut server_metrics = self.server_metrics.write().await;
        *server_metrics = Some(metrics);
    }

    pub fn record_query(&mut self, response_time: Duration) {
        self.query_count += 1;
        self.total_response_time += response_time;

        // ✅ ИСПРАВЛЕНО: Убрали .await из sync метода
        if self.last_update.elapsed() >= Duration::from_secs(1) || self.query_count >= 1000 {
            // Используем tokio::spawn для асинхронной эмиссии метрик
            let sender = self.metrics_sender.clone();
            let server_metrics = self.server_metrics.clone();
            let query_count = self.query_count;
            let total_response_time = self.total_response_time;
            let error_count = self.error_count;
            let last_update = self.last_update;

            tokio::spawn(async move {
                Self::emit_metrics_async(
                    sender,
                    server_metrics,
                    query_count,
                    total_response_time,
                    error_count,
                    last_update
                ).await;
            });

            // Сбрасываем счетчики
            self.last_update = Instant::now();
            self.query_count = 0;
            self.total_response_time = Duration::default();
            self.error_count = 0;
        }
    }

    pub fn record_error(&mut self) {
        self.error_count += 1;
    }

    // ✅ ИСПРАВЛЕНО: Вынесли async логику в отдельный метод
    async fn emit_metrics_async(
        sender: broadcast::Sender<RealTimeMetrics>,
        server_metrics: Arc<RwLock<Option<Arc<super::metrics::MetricsCollector>>>>,
        query_count: u64,
        total_response_time: Duration,
        error_count: u64,
        last_update: Instant,
    ) {
        let elapsed = last_update.elapsed().as_secs_f64();
        let qps = query_count as f64 / elapsed.max(0.1);
        let avg_response_time = if query_count > 0 {
            total_response_time.as_secs_f64() / query_count as f64
        } else {
            0.0
        };

        let error_rate = if query_count > 0 {
            error_count as f64 / query_count as f64
        } else {
            0.0
        };

        let (cache_hit_rate, active_connections) = Self::get_server_metrics(&server_metrics).await;

        let metrics = RealTimeMetrics {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            queries_per_second: qps,
            average_response_time: avg_response_time * 1000.0,
            active_connections,
            cache_hit_rate,
            memory_usage_mb: Self::get_memory_usage(),
            error_rate: error_rate * 100.0,
            replica_lag_ms: None,
        };

        let _ = sender.send(metrics);
    }

    async fn get_server_metrics(
        server_metrics: &Arc<RwLock<Option<Arc<super::metrics::MetricsCollector>>>>
    ) -> (f64, usize) {
        let server_metrics_guard = server_metrics.read().await;
        if let Some(metrics) = server_metrics_guard.as_ref() {
            let stats = metrics.get_metrics();
            let cache_hit_rate = if stats.cache_hits + stats.cache_misses > 0 {
                stats.cache_hits as f64 / (stats.cache_hits + stats.cache_misses) as f64
            } else {
                0.0
            };
            (cache_hit_rate, stats.active_connections)
        } else {
            (0.0, 0)
        }
    }

    fn get_memory_usage() -> f64 {
        #[cfg(target_os = "linux")]
        {
            if let Ok(contents) = std::fs::read_to_string("/proc/self/statm") {
                if let Some(size) = contents.split_whitespace().next() {
                    if let Ok(pages) = size.parse::<f64>() {
                        return pages * 4096.0 / 1024.0 / 1024.0;
                    }
                }
            }
        }
        0.0
    }

    // ✅ ИСПРАВЛЕНО: Убрали mut из ненужного места
    pub async fn start_background_monitoring(self: Arc<Self>) {
        let monitor = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                // Периодическая эмиссия метрик даже без активности
                if monitor.query_count > 0 {
                    // Клонируем данные для асинхронной обработки
                    let sender = monitor.metrics_sender.clone();
                    let server_metrics = monitor.server_metrics.clone();
                    let query_count = monitor.query_count;
                    let total_response_time = monitor.total_response_time;
                    let error_count = monitor.error_count;
                    let last_update = monitor.last_update;

                    tokio::spawn(async move {
                        Self::emit_metrics_async(
                            sender,
                            server_metrics,
                            query_count,
                            total_response_time,
                            error_count,
                            last_update
                        ).await;
                    });
                }
            }
        });
    }
}

pub struct AlertSystem {
    alert_thresholds: AlertThresholds,
    alert_sender: broadcast::Sender<Alert>,
    triggered_alerts: Arc<RwLock<Vec<Alert>>>,
}

#[derive(Debug, Clone)]
pub struct Alert {
    pub level: AlertLevel,
    pub message: String,
    pub timestamp: u64,
    pub resolved: bool,
}

#[derive(Debug, Clone)]
pub enum AlertLevel {
    Info,
    Warning,
    Critical,
    Error,
}

#[derive(Debug, Clone)]
pub struct AlertThresholds {
    pub max_qps: f64,
    pub max_response_time_ms: f64,
    pub max_memory_mb: f64,
    pub min_cache_hit_rate: f64,
    pub max_error_rate: f64,
    pub max_replica_lag_ms: f64,
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self {
            max_qps: 1000.0,
            max_response_time_ms: 1000.0,
            max_memory_mb: 1024.0,
            min_cache_hit_rate: 0.8,
            max_error_rate: 5.0,
            max_replica_lag_ms: 5000.0,
        }
    }
}

impl AlertSystem {
    pub fn new(thresholds: AlertThresholds) -> (Self, broadcast::Receiver<Alert>) {
        let (sender, receiver) = broadcast::channel(50);

        let system = Self {
            alert_thresholds: thresholds,
            alert_sender: sender,
            triggered_alerts: Arc::new(RwLock::new(Vec::new())),
        };

        (system, receiver)
    }

    pub async fn check_metrics(&self, metrics: &RealTimeMetrics, server_metrics: &super::metrics::ServerMetrics) {
        let mut alerts = Vec::new();

        if metrics.queries_per_second > self.alert_thresholds.max_qps {
            alerts.push(self.create_alert(
                AlertLevel::Warning,
                format!("High QPS: {:.2} queries/second", metrics.queries_per_second)
            ));
        }

        if metrics.average_response_time > self.alert_thresholds.max_response_time_ms {
            alerts.push(self.create_alert(
                AlertLevel::Critical,
                format!("Slow response time: {:.2}ms", metrics.average_response_time)
            ));
        }

        if metrics.memory_usage_mb > self.alert_thresholds.max_memory_mb {
            alerts.push(self.create_alert(
                AlertLevel::Error,
                format!("High memory usage: {:.2}MB", metrics.memory_usage_mb)
            ));
        }

        let cache_hit_rate = if server_metrics.cache_hits + server_metrics.cache_misses > 0 {
            server_metrics.cache_hits as f64 / (server_metrics.cache_hits + server_metrics.cache_misses) as f64
        } else {
            1.0
        };

        if cache_hit_rate < self.alert_thresholds.min_cache_hit_rate {
            alerts.push(self.create_alert(
                AlertLevel::Warning,
                format!("Low cache hit rate: {:.2}%", cache_hit_rate * 100.0)
            ));
        }

        if metrics.error_rate > self.alert_thresholds.max_error_rate {
            alerts.push(self.create_alert(
                AlertLevel::Error,
                format!("High error rate: {:.2}%", metrics.error_rate)
            ));
        }

        if let Some(lag) = metrics.replica_lag_ms {
            if lag > self.alert_thresholds.max_replica_lag_ms {
                alerts.push(self.create_alert(
                    AlertLevel::Warning,
                    format!("High replica lag: {:.2}ms", lag)
                ));
            }
        }

        for alert in alerts {
            self.send_alert(alert).await;
        }
    }

    fn create_alert(&self, level: AlertLevel, message: String) -> Alert {
        Alert {
            level,
            message,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            resolved: false,
        }
    }

    async fn send_alert(&self, alert: Alert) {
        {
            let mut triggered_alerts = self.triggered_alerts.write().await;
            triggered_alerts.push(alert.clone());
            if triggered_alerts.len() > 1000 {
                triggered_alerts.drain(0..100);
            }
        }

        let _ = self.alert_sender.send(alert);
    }

    pub async fn resolve_alert(&self, timestamp: u64) {
        let mut triggered_alerts = self.triggered_alerts.write().await;
        if let Some(alert) = triggered_alerts.iter_mut().find(|a| a.timestamp == timestamp) {
            alert.resolved = true;
        }
    }

    pub async fn get_alert_history(&self) -> Vec<Alert> {
        let triggered_alerts = self.triggered_alerts.read().await;
        triggered_alerts.clone()
    }

    pub async fn get_active_alerts(&self) -> Vec<Alert> {
        let triggered_alerts = self.triggered_alerts.read().await;
        triggered_alerts.iter()
            .filter(|alert| !alert.resolved)
            .cloned()
            .collect()
    }
}