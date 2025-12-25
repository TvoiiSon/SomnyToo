use async_trait::async_trait;
use std::path::Path;
use tracing::warn;

use super::super::health_check::{HealthCheckable, HealthStatus, ComponentType};

pub struct DiskHealthCheck;

impl DiskHealthCheck {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl HealthCheckable for DiskHealthCheck {
    async fn health_check(&self) -> HealthStatus {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let check_path = if cfg!(windows) {
            Path::new("C:")
        } else {
            Path::new("/")
        };

        // Временно используем sys_info для проверки диска (более простая реализация)
        match sys_info::disk_info() {
            Ok(disk_info) => {
                let total_gb = disk_info.total as f64 / 1024.0 / 1024.0 / 1024.0;
                let free_gb = disk_info.free as f64 / 1024.0 / 1024.0 / 1024.0;
                let used_gb = total_gb - free_gb;
                let usage_percent = (used_gb / total_gb) * 100.0;

                let is_healthy = usage_percent < 90.0;
                let status = if usage_percent < 70.0 {
                    "Normal"
                } else if usage_percent < 85.0 {
                    "High"
                } else {
                    "Critical"
                };

                if !is_healthy {
                    warn!("Disk usage critical: {:.1}% used", usage_percent);
                }

                HealthStatus {
                    is_healthy,
                    message: format!("Disk: {:.1}% used ({:.1}GB free) on {:?}",
                                     usage_percent, free_gb, check_path),
                    details: Some(serde_json::json!({
                        "total_gb": total_gb,
                        "free_gb": free_gb,
                        "used_percent": usage_percent,
                        "status": status,
                        "checked_path": check_path.to_string_lossy().to_string()
                    })),
                    timestamp,
                }
            },
            Err(e) => {
                HealthStatus {
                    is_healthy: false,
                    message: format!("Cannot read disk info for {:?}: {}", check_path, e),
                    details: None,
                    timestamp,
                }
            }
        }
    }

    fn component_name(&self) -> &'static str {
        "disk"
    }

    fn component_type(&self) -> ComponentType {
        ComponentType::Storage
    }
}