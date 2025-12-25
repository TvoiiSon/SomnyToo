use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;
use serde::Serialize;
use tracing::{info, debug};

use super::health_check::{HealthCheckable, HealthStatus, ComponentType};
use super::config::MonitoringConfig;

#[derive(Debug, Serialize, Clone)]
pub struct SystemHealthReport {
    pub overall_health: bool,
    pub components: HashMap<String, HealthStatus>,
    pub critical_components: Vec<String>,
    pub warnings: Vec<String>,
    pub timestamp: u64,
}

pub struct UnifiedMonitor {
    monitors: Arc<RwLock<HashMap<String, Arc<dyn Monitor>>>>,
    health_checks: Arc<RwLock<HashMap<String, Arc<dyn HealthCheckable>>>>, // Добавляем поле
    alerts: Arc<RwLock<VecDeque<Alert>>>,
    config: MonitoringConfig,
    health_history: Arc<RwLock<VecDeque<SystemHealthReport>>>, // Добавляем поле
}

impl UnifiedMonitor {
    pub fn new(config: MonitoringConfig) -> Self {
        Self {
            monitors: Arc::new(RwLock::new(HashMap::new())),
            health_checks: Arc::new(RwLock::new(HashMap::new())), // Инициализируем
            alerts: Arc::new(RwLock::new(VecDeque::with_capacity(config.max_alerts))),
            config,
            health_history: Arc::new(RwLock::new(VecDeque::with_capacity(100))), // Инициализируем
        }
    }

    pub fn get_config(&self) -> &MonitoringConfig {
        &self.config
    }

    async fn cleanup_old_data(&self) {
        self.cleanup_old_alerts().await;
        self.cleanup_old_health_history().await;
    }

    async fn record_internal_metrics(&self) {
        let health_checks = self.health_checks.read().await;
        let monitors = self.monitors.read().await;
        let alerts = self.alerts.read().await;
        let health_history_size = self.health_history.read().await.len();

        // Эти метрики можно экспортировать в Prometheus
        debug!(
            health_checks_count = health_checks.len(),
            monitors_count = monitors.len(),
            alerts_count = alerts.len(),
            health_history_size = health_history_size,
            "Monitoring internal stats"
        );
    }

    // Существующие методы для мониторов
    pub async fn register_monitor(&self, monitor: Arc<dyn Monitor>) {
        let mut monitors = self.monitors.write().await;
        monitors.insert(monitor.name().to_string(), monitor.clone());
        info!("Monitor registered: {}", monitor.name());
    }

    pub async fn get_monitor(&self, name: &str) -> Option<Arc<dyn Monitor>> {
        let monitors = self.monitors.read().await;
        monitors.get(name).cloned()
    }

    pub async fn add_alert(&self, level: AlertLevel, source: &str, message: &str) {
        if !self.config.enabled {
            return;
        }

        use std::sync::atomic::{AtomicUsize, Ordering};
        static CLEANUP_COUNTER: AtomicUsize = AtomicUsize::new(0);

        let count = CLEANUP_COUNTER.fetch_add(1, Ordering::Relaxed);
        if count % 100 == 0 {
            self.cleanup_old_alerts().await;
        }

        let alert = Alert {
            level: level.clone(),
            source: source.to_string(),
            message: message.to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            metadata: HashMap::new(),
        };

        let mut alerts = self.alerts.write().await;

        // Используем max_alerts из конфига
        if alerts.len() >= self.config.max_alerts {
            alerts.pop_front();
        }

        alerts.push_back(alert.clone());

        // Логирование в зависимости от уровня и конфига
        if self.config.alerting_enabled {
            match level {
                AlertLevel::Info => info!("[{}] {}", source, message),
                AlertLevel::Warning => tracing::warn!("[{}] {}", source, message),
                AlertLevel::Error => tracing::error!("[{}] {}", source, message),
                AlertLevel::Critical => {
                    tracing::error!("🚨 CRITICAL [{}] {}", source, message);
                }
            }
        }
    }

    pub async fn collect_all_metrics(&self) -> HashMap<String, MonitorMetrics> {
        let monitors = self.monitors.read().await;
        let mut all_metrics = HashMap::new();

        for (name, monitor) in monitors.iter() {
            let metrics = monitor.collect_metrics().await;
            all_metrics.insert(name.clone(), metrics);
        }

        all_metrics
    }

    pub async fn overall_health(&self) -> bool {
        let monitors = self.monitors.read().await;
        for monitor in monitors.values() {
            if !monitor.health_check().await {
                return false;
            }
        }
        true
    }

    pub async fn get_prometheus_metrics(&self) -> String {
        if !self.config.prometheus_enabled {
            return String::new();
        }

        let all_metrics = self.collect_all_metrics().await;
        let mut prometheus_output = String::new();

        for (monitor_name, metrics) in all_metrics {
            for (metric_name, metric_value) in metrics.metrics {
                let metric_line = match metric_value {
                    MetricValue::Counter(value) => {
                        format!("{}_{}{{monitor=\"{}\"}} {}\n",
                                monitor_name, metric_name, monitor_name, value)
                    }
                    MetricValue::Gauge(value) => {
                        format!("{}_{}{{monitor=\"{}\"}} {}\n",
                                monitor_name, metric_name, monitor_name, value)
                    }
                    MetricValue::Boolean(value) => {
                        format!("{}_{}{{monitor=\"{}\"}} {}\n",
                                monitor_name, metric_name, monitor_name, if value { 1 } else { 0 })
                    }
                    _ => continue,
                };
                prometheus_output.push_str(&metric_line);
            }
        }

        prometheus_output
    }

    pub async fn get_recent_alerts(&self, limit: usize) -> Vec<Alert> {
        let alerts = self.alerts.read().await;
        alerts.iter().rev().take(limit).cloned().collect()
    }

    pub async fn reset_all_metrics(&self) {
        let monitors = self.monitors.read().await;
        for monitor in monitors.values() {
            monitor.reset_metrics().await;
        }
        info!("All metrics reset");
    }

    // Новые методы для health checks
    pub async fn register_health_check(&self, component: Arc<dyn HealthCheckable>) {
        let mut health_checks = self.health_checks.write().await;
        health_checks.insert(component.component_name().to_string(), component.clone());
        info!("Health check registered: {}", component.component_name());
    }

    pub async fn comprehensive_health_check(&self) -> SystemHealthReport {
        let health_checks = self.health_checks.read().await;
        let start_time = std::time::Instant::now();

        // Собираем все futures для параллельного выполнения
        let mut health_futures = Vec::new();
        let mut component_names = Vec::new();

        for (name, health_check) in health_checks.iter() {
            let name_clone = name.clone();
            let health_check_clone = health_check.clone();

            health_futures.push(async move {
                let status = health_check_clone.health_check().await;
                (name_clone, health_check_clone, status)
            });
            component_names.push(name.clone());
        }

        // Выполняем все проверки параллельно
        let results = futures::future::join_all(health_futures).await;

        let mut components = HashMap::new();
        let mut critical_components = Vec::new();
        let mut warnings = Vec::new();

        for (name, health_check, status) in results {
            if !status.is_healthy {
                match health_check.component_type() {
                    ComponentType::Database | ComponentType::ExternalService => {
                        critical_components.push(name.clone());
                    },
                    _ => {
                        warnings.push(format!("{}: {}", name, status.message));
                    }
                }
            }

            components.insert(name, status);
        }

        let overall_health = critical_components.is_empty();

        let report = SystemHealthReport {
            overall_health,
            components,
            critical_components,
            warnings,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };

        // Сохраняем в историю
        let mut history = self.health_history.write().await;
        history.push_back(report.clone());
        if history.len() > 50 {
            history.pop_front();
        }

        // Периодически чистим старые данные
        static CLEANUP_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let count = CLEANUP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count % 10 == 0 {
            self.cleanup_old_data().await;
        }

        let duration = start_time.elapsed();
        debug!(
            duration_ms = duration.as_millis(),
            "Health check completed"
        );

        // Записываем внутренние метрики
        self.record_internal_metrics().await;

        report
    }

    pub async fn get_health_history(&self, limit: usize) -> Vec<SystemHealthReport> {
        let history = self.health_history.read().await;
        history.iter().rev().take(limit).cloned().collect()
    }

    pub async fn critical_health_check(&self) -> bool {
        let report = self.comprehensive_health_check().await;
        report.critical_components.is_empty()
    }

    pub async fn generate_web_report(&self) -> serde_json::Value {
        let health_report = self.comprehensive_health_check().await;
        let metrics = self.collect_all_metrics().await;
        let recent_alerts = self.get_recent_alerts(10).await;

        serde_json::json!({
            "system_health": health_report,
            "metrics": metrics,
            "recent_alerts": recent_alerts,
            "server_info": {
                "uptime": self.get_uptime(),
                "version": env!("CARGO_PKG_VERSION"),
                "timestamp": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            }
        })
    }

    async fn cleanup_old_alerts(&self) {
        let retention_days = self.config.retention_days;
        if retention_days == 0 {
            return; // Бесконечное хранение
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let retention_seconds = (retention_days as u64) * 24 * 60 * 60;

        let mut alerts = self.alerts.write().await;
        alerts.retain(|alert| {
            now - alert.timestamp <= retention_seconds
        });
    }

    async fn cleanup_old_health_history(&self) {
        let retention_days = self.config.retention_days;
        if retention_days == 0 {
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let retention_seconds = (retention_days as u64) * 24 * 60 * 60;

        let mut history = self.health_history.write().await;
        history.retain(|report| {
            now - report.timestamp <= retention_seconds
        });
    }

    fn get_uptime(&self) -> u64 {
        // В реальной реализации здесь будет расчет uptime
        // Пока заглушка
        0
    }
}

// Остальные структуры и enum'ы
#[derive(Debug, Serialize, Clone)]
pub struct MonitorMetrics {
    pub name: String,
    pub timestamp: u64,
    pub metrics: HashMap<String, MetricValue>,
    pub health: bool,
}

#[derive(Debug, Serialize, Clone)]
pub enum MetricValue {
    Counter(u64),
    Gauge(f64),
    Histogram(Vec<f64>),
    String(String),
    Boolean(bool),
}

#[derive(Debug, Serialize, Clone)]
pub struct Alert {
    pub level: AlertLevel,
    pub source: String,
    pub message: String,
    pub timestamp: u64,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub enum AlertLevel {
    Info,
    Warning,
    Error,
    Critical,
}

#[async_trait::async_trait]
pub trait Monitor: Send + Sync {
    fn name(&self) -> &'static str;
    async fn collect_metrics(&self) -> MonitorMetrics;
    async fn health_check(&self) -> bool;
    async fn reset_metrics(&self);
}