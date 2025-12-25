use async_trait::async_trait;
use serde::Serialize;
use std::sync::Arc;

use super::unified_monitor::Monitor;

#[derive(Debug, Serialize, Clone)]
pub struct HealthStatus {
    pub is_healthy: bool,
    pub message: String,
    pub details: Option<serde_json::Value>,
    pub timestamp: u64,
}

/// Адаптер для использования мониторов как health checks без циклических зависимостей
pub struct MonitorHealthAdapter<T: Monitor> {
    monitor: Arc<T>,
    component_type: ComponentType,
}

impl<T: Monitor> MonitorHealthAdapter<T> {
    pub fn new(monitor: Arc<T>, component_type: ComponentType) -> Self {
        Self {
            monitor,
            component_type,
        }
    }
}

#[async_trait]
pub trait HealthCheckable: Send + Sync {
    async fn health_check(&self) -> HealthStatus;

    fn component_name(&self) -> &'static str;

    fn component_type(&self) -> ComponentType;
}

#[derive(Debug, Serialize, Clone)]
pub enum ComponentType {
    Database,
    Service,
    Network,
    Storage,
    ExternalService,
    Custom(&'static str),
}

// Базовая реализация для всех мониторов
#[async_trait]
impl<T: Monitor + Sync + Send> HealthCheckable for MonitorHealthAdapter<T> {
    async fn health_check(&self) -> HealthStatus {
        let is_healthy = self.monitor.health_check().await;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        HealthStatus {
            is_healthy,
            message: if is_healthy {
                format!("{} is healthy", self.monitor.name())
            } else {
                format!("{} is unhealthy", self.monitor.name())
            },
            details: None,
            timestamp,
        }
    }

    fn component_name(&self) -> &'static str {
        self.monitor.name()
    }

    fn component_type(&self) -> ComponentType {
        self.component_type.clone()
    }
}