use async_trait::async_trait;
use std::net::{ToSocketAddrs};
use std::time::Duration;
use tracing::{warn};

use super::super::health_check::{HealthCheckable, HealthStatus, ComponentType};

pub struct NetworkHealthCheck;

impl NetworkHealthCheck {
    pub fn new() -> Self {
        Self
    }

    async fn check_port_async(&self, host: &str, port: u16, timeout_secs: u64) -> Result<Duration, String> {
        let start = std::time::Instant::now();

        let addr_string = format!("{}:{}", host, port);
        let addresses: Vec<std::net::SocketAddr> = match addr_string.to_socket_addrs() {
            Ok(addrs) => addrs.collect(),
            Err(e) => return Err(format!("Cannot resolve {}:{} - {}", host, port, e)),
        };

        if addresses.is_empty() {
            return Err(format!("No addresses found for {}:{}", host, port));
        }

        // Используем асинхронный TcpStream
        match tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            tokio::net::TcpStream::connect(&addresses[0])
        ).await {
            Ok(Ok(_)) => {
                let duration = start.elapsed();
                Ok(duration)
            },
            Ok(Err(e)) => Err(format!("Cannot connect to {}:{} - {}", host, port, e)),
            Err(_) => Err(format!("Connection timeout to {}:{}", host, port)),
        }
    }

    async fn check_external_connectivity_async(&self) -> Result<Vec<(String, Duration)>, String> {
        let mut results = Vec::new();
        let mut errors = Vec::new();

        let targets = vec![
            ("google.com", 80),
            ("cloudflare.com", 80),
            ("github.com", 80),
        ];

        let mut futures = Vec::new();

        for (host, port) in targets {
            let host = host.to_string();
            futures.push(async move {
                match self.check_port_async(&host, port, 3).await {
                    Ok(duration) => Ok((format!("{}:{}", host, port), duration)),
                    Err(e) => Err(format!("{}:{} - {}", host, port, e)),
                }
            });
        }

        let check_results = futures::future::join_all(futures).await;

        for result in check_results {
            match result {
                Ok(success) => results.push(success),
                Err(error) => errors.push(error),
            }
        }

        // Возвращаем результаты даже если есть ошибки, но логируем предупреждение
        if !errors.is_empty() {
            warn!("Some connectivity checks failed: {:?}", errors);
        }

        // Если все проверки провалились - возвращаем ошибку
        if results.is_empty() {
            return Err(format!("All connectivity checks failed: {:?}", errors));
        }

        Ok(results)
    }
}

#[async_trait]
impl HealthCheckable for NetworkHealthCheck {
    async fn health_check(&self) -> HealthStatus {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        match self.check_external_connectivity_async().await {
            Ok(results) => {
                if results.is_empty() {
                    return HealthStatus {
                        is_healthy: false,
                        message: "No network targets could be reached".to_string(),
                        details: None,
                        timestamp,
                    };
                }

                let avg_latency = results.iter()
                    .map(|(_, duration)| duration.as_millis())
                    .sum::<u128>() / results.len() as u128;

                let success_rate = (results.len() as f64 / 3.0) * 100.0; // 3 цели всего

                HealthStatus {
                    is_healthy: success_rate > 50.0, // Здорово если >50% целей доступны
                    message: format!("Network: {}/3 targets, avg latency {}ms",
                                     results.len(), avg_latency),
                    details: Some(serde_json::json!({
                        "targets_checked": 3,
                        "targets_reachable": results.len(),
                        "success_rate_percent": success_rate,
                        "average_latency_ms": avg_latency,
                        "connections": results.into_iter()
                            .map(|(target, duration)| {
                                serde_json::json!({
                                    "target": target,
                                    "latency_ms": duration.as_millis()
                                })
                            })
                            .collect::<Vec<_>>()
                    })),
                    timestamp,
                }
            },
            Err(e) => {
                warn!("Network health check failed: {}", e);
                HealthStatus {
                    is_healthy: false,
                    message: format!("Network issues: {}", e),
                    details: None,
                    timestamp,
                }
            }
        }
    }

    fn component_name(&self) -> &'static str {
        "network"
    }

    fn component_type(&self) -> ComponentType {
        ComponentType::Network
    }
}