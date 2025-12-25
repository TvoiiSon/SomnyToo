use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MonitoringConfig {
    pub enabled: bool,
    pub prometheus_enabled: bool,
    pub prometheus_port: u16,
    pub alerting_enabled: bool,
    pub metrics_interval_sec: u64,
    pub retention_days: u32,
    pub max_alerts: usize,
}

impl Default for MonitoringConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            prometheus_enabled: true,
            prometheus_port: 9090,
            alerting_enabled: true,
            metrics_interval_sec: 60,
            retention_days: 30,
            max_alerts: 1000,
        }
    }
}