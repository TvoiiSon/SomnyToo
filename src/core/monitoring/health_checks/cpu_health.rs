use async_trait::async_trait;
use tracing::warn;

use super::super::health_check::{HealthCheckable, HealthStatus, ComponentType};

pub struct CpuHealthCheck;

impl CpuHealthCheck {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl HealthCheckable for CpuHealthCheck {
    async fn health_check(&self) -> HealthStatus {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        match sys_info::loadavg() {
            Ok(loadavg) => {
                let cpu_count = num_cpus::get() as f64;
                let load_1_percent = (loadavg.one / cpu_count) * 100.0;
                let load_5_percent = (loadavg.five / cpu_count) * 100.0;

                let is_healthy = load_1_percent < 80.0; // Критический порог - 80%
                let status = if load_1_percent < 50.0 {
                    "Normal"
                } else if load_1_percent < 70.0 {
                    "High"
                } else {
                    "Critical"
                };

                if !is_healthy {
                    warn!("CPU load critical: {:.1}% (1min)", load_1_percent);
                }

                HealthStatus {
                    is_healthy,
                    message: format!("CPU Load: {:.1}% (1min), {:.1}% (5min)",
                                     load_1_percent, load_5_percent),
                    details: Some(serde_json::json!({
                        "load_1min": loadavg.one,
                        "load_5min": loadavg.five,
                        "load_15min": loadavg.fifteen,
                        "load_1min_percent": load_1_percent,
                        "load_5min_percent": load_5_percent,
                        "cpu_cores": cpu_count,
                        "status": status
                    })),
                    timestamp,
                }
            },
            Err(e) => {
                HealthStatus {
                    is_healthy: false,
                    message: format!("Cannot read CPU info: {}", e),
                    details: None,
                    timestamp,
                }
            }
        }
    }

    fn component_name(&self) -> &'static str {
        "cpu"
    }

    fn component_type(&self) -> ComponentType {
        ComponentType::Service
    }
}