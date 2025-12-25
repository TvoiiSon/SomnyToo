use async_trait::async_trait;
use tokio::time::{timeout, Duration, error::Elapsed};
use tracing::{error, debug};

use crate::core::sql_server::executor::{QUERY_EXECUTOR, QueryResult};
use super::super::health_check::{HealthCheckable, HealthStatus, ComponentType};

pub struct DatabaseHealthCheck;

impl DatabaseHealthCheck {
    pub fn new() -> Self {
        Self
    }
    async fn execute_health_query_with_retry(&self) -> Result<(bool, u128), String> {
        const MAX_RETRIES: u32 = 3;
        const INITIAL_DELAY_MS: u64 = 100;

        let mut last_error = None;

        for attempt in 0..MAX_RETRIES {
            match self.execute_single_health_query().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    last_error = Some(e);

                    if attempt < MAX_RETRIES - 1 {
                        let delay_ms = INITIAL_DELAY_MS * 2u64.pow(attempt); // Exponential backoff
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        debug!("Database health check retry {}/{} after {}ms",
                               attempt + 1, MAX_RETRIES, delay_ms);
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| "All retry attempts failed".to_string()))
    }

    async fn execute_single_health_query(&self) -> Result<(bool, u128), String> {
        let start = std::time::Instant::now();

        let test_query = "SELECT 1 as health_check";
        let result: Result<QueryResult, Elapsed> = timeout(
            Duration::from_secs(5),
            QUERY_EXECUTOR.execute_query(test_query, "health_check")
        ).await;

        let query_time = start.elapsed().as_millis();

        match result {
            Ok(query_result) => {
                if query_result.success {
                    Ok((true, query_time))
                } else {
                    Err(format!("Query failed: {:?}", query_result.error_message))
                }
            },
            Err(_) => Err("Database query timeout".to_string()),
        }
    }
}

#[async_trait]
impl HealthCheckable for DatabaseHealthCheck {
    async fn health_check(&self) -> HealthStatus {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        match self.execute_health_query_with_retry().await {
            Ok((_, query_time)) => {
                let status = if query_time < 100 {
                    "Excellent"
                } else if query_time < 500 {
                    "Good"
                } else if query_time < 1000 {
                    "Slow"
                } else {
                    "Critical"
                };

                HealthStatus {
                    is_healthy: query_time < 2000, // 2 секунды - максимум
                    message: format!("Database response time: {}ms ({})", query_time, status),
                    details: Some(serde_json::json!({
                        "response_time_ms": query_time,
                        "status": status,
                        "connection_pool": "active"
                    })),
                    timestamp,
                }
            },
            Err(e) => {
                error!("Database health check failed: {}", e);
                HealthStatus {
                    is_healthy: false,
                    message: format!("Database unreachable: {}", e),
                    details: None,
                    timestamp,
                }
            }
        }
    }

    fn component_name(&self) -> &'static str {
        "database"
    }

    fn component_type(&self) -> ComponentType {
        ComponentType::Database
    }
}