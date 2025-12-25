use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use super::super::health_check::{HealthCheckable, HealthStatus, ComponentType};
use super::super::system_info::{SystemInfoProvider, RealSystemInfo};

// Простой кэш для хранения данных памяти
#[derive(Clone)]
struct SimpleCache {
    data: Arc<RwLock<Option<(std::time::Instant, serde_json::Value)>>>,
    ttl: Duration,
}

impl SimpleCache {
    fn new(ttl: Duration) -> Self {
        Self {
            data: Arc::new(RwLock::new(None)),
            ttl,
        }
    }

    async fn get_or_compute<F, Fut>(&self, computer: F) -> serde_json::Value
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = serde_json::Value>,
    {
        let now = std::time::Instant::now();
        let mut cache = self.data.write().await;

        // Проверяем, есть ли валидные данные в кэше
        if let Some((timestamp, value)) = cache.as_ref() {
            if now.duration_since(*timestamp) < self.ttl {
                return value.clone();
            }
        }

        // Вычисляем новое значение
        let value = computer().await;
        *cache = Some((now, value.clone()));

        value
    }
}

pub struct MemoryHealthCheck {
    cache: SimpleCache,
    system_info: Arc<dyn SystemInfoProvider>,
}

impl MemoryHealthCheck {
    pub fn new() -> Self {
        Self {
            cache: SimpleCache::new(Duration::from_secs(10)),
            system_info: Arc::new(RealSystemInfo),
        }
    }

    // Для тестов
    #[cfg(test)]
    pub fn with_mock(system_info: Arc<dyn SystemInfoProvider>) -> Self {
        Self {
            cache: SimpleCache::new(Duration::from_secs(10)),
            system_info,
        }
    }

    async fn get_memory_info_cached(&self) -> serde_json::Value {
        self.cache.get_or_compute(|| async {
            match self.system_info.get_memory_info().await {
                Ok(mem_info) => {
                    if mem_info.total == 0 {
                        return serde_json::json!({
                            "error": "Memory info unavailable - total memory is 0",
                            "data_quality": "unavailable"
                        });
                    }

                    let total_mb = mem_info.total / 1024;
                    let available_mb = mem_info.avail / 1024;
                    let used_mb = total_mb.saturating_sub(available_mb);
                    let usage_percent = (used_mb as f64 / total_mb as f64) * 100.0;

                    let (is_healthy, status) = if usage_percent > 95.0 {
                        (true, "Data Questionable")
                    } else if usage_percent < 80.0 {
                        (true, "Normal")
                    } else if usage_percent < 90.0 {
                        (true, "High")
                    } else {
                        (false, "Critical")
                    };

                    serde_json::json!({
                        "total_mb": total_mb,
                        "used_mb": used_mb,
                        "available_mb": available_mb,
                        "usage_percent": usage_percent,
                        "is_healthy": is_healthy,
                        "status": status,
                        "data_quality": if usage_percent > 95.0 { "questionable" } else { "good" }
                    })
                },
                Err(e) => serde_json::json!({
                    "error": format!("Cannot read memory info: {}", e),
                    "data_quality": "error"
                })
            }
        }).await
    }
}

#[async_trait]
impl HealthCheckable for MemoryHealthCheck {
    async fn health_check(&self) -> HealthStatus {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let cached_info = self.get_memory_info_cached().await;

        if let Some(error_value) = cached_info.get("error") {
            if let Some(error_str) = error_value.as_str() {
                return HealthStatus {
                    is_healthy: true,
                    message: error_str.to_string(),
                    details: Some(serde_json::json!({
                        "data_quality": cached_info.get("data_quality").unwrap_or(&serde_json::Value::Null)
                    })),
                    timestamp,
                };
            }
        }

        let total_mb = cached_info["total_mb"].as_u64().unwrap_or(0);
        let used_mb = cached_info["used_mb"].as_u64().unwrap_or(0);
        let available_mb = cached_info["available_mb"].as_u64().unwrap_or(0);
        let usage_percent = cached_info["usage_percent"].as_f64().unwrap_or(0.0);
        let is_healthy = cached_info["is_healthy"].as_bool().unwrap_or(true);
        let status = cached_info["status"].as_str().unwrap_or("Unknown");
        let data_quality = cached_info["data_quality"].as_str().unwrap_or("unknown");

        if !is_healthy {
            tracing::warn!(
                usage_percent = usage_percent,
                total_mb = total_mb,
                available_mb = available_mb,
                "Memory usage critical"
            );
        }

        HealthStatus {
            is_healthy,
            message: format!("Memory: {:.1}% used ({})", usage_percent, status),
            details: Some(serde_json::json!({
                "total_mb": total_mb,
                "used_mb": used_mb,
                "available_mb": available_mb,
                "usage_percent": usage_percent,
                "status": status,
                "data_quality": data_quality
            })),
            timestamp,
        }
    }

    fn component_name(&self) -> &'static str {
        "memory"
    }

    fn component_type(&self) -> ComponentType {
        ComponentType::Storage
    }
}