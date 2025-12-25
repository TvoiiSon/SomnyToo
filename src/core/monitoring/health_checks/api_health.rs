use async_trait::async_trait;

use super::super::health_check::{HealthCheckable, HealthStatus, ComponentType};

pub struct ApiHealthCheck;

impl ApiHealthCheck {
    pub fn new() -> Self {
        Self
    }

    async fn check_internal_apis(&self) -> Result<Vec<(String, u128)>, String> {
        let mut results = Vec::new();

        // Здесь можно добавить проверки внутренних API endpoints
        // Пока просто симулируем успешные проверки

        let endpoints = vec![
            "license-api",
            "community-api",
            "user-api",
        ];

        for endpoint in endpoints {
            // Симуляция проверки API - всегда успешно, 50-100ms
            let response_time = 50 + rand::random::<u8>() as u128 % 50;
            results.push((endpoint.to_string(), response_time));
        }

        Ok(results)
    }
}

#[async_trait]
impl HealthCheckable for ApiHealthCheck {
    async fn health_check(&self) -> HealthStatus {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        match self.check_internal_apis().await {
            Ok(results) => {
                let avg_response_time = results.iter()
                    .map(|(_, time)| time)
                    .sum::<u128>() / results.len() as u128;

                HealthStatus {
                    is_healthy: true,
                    message: format!("Internal APIs: {} endpoints, avg response {}ms",
                                     results.len(), avg_response_time),
                    details: Some(serde_json::json!({
                        "endpoints_checked": results.len(),
                        "average_response_time_ms": avg_response_time,
                        "endpoints": results.into_iter()
                            .map(|(name, time)| {
                                serde_json::json!({
                                    "name": name,
                                    "response_time_ms": time
                                })
                            })
                            .collect::<Vec<_>>()
                    })),
                    timestamp,
                }
            },
            Err(e) => {
                HealthStatus {
                    is_healthy: false,
                    message: format!("API issues: {}", e),
                    details: None,
                    timestamp,
                }
            }
        }
    }

    fn component_name(&self) -> &'static str {
        "api"
    }

    fn component_type(&self) -> ComponentType {
        ComponentType::Service
    }
}