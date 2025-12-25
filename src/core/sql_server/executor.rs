use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use super::server::{SqlServer, QueryResult as ServerQueryResult};

pub struct QueryExecutor {
    server: Arc<Mutex<Option<SqlServer>>>,
}

#[derive(Debug)]
pub struct QueryResult {
    pub rows_affected: u64,
    pub execution_time: Duration,
    pub success: bool,
    pub error_message: Option<String>,
    pub used_replica: bool,
    pub used_cache: bool,
    pub cached_result: Option<String>,
}

// Конвертация из ServerQueryResult в наш QueryResult
impl From<ServerQueryResult> for QueryResult {
    fn from(server_result: ServerQueryResult) -> Self {
        Self {
            rows_affected: server_result.rows_affected,
            execution_time: server_result.execution_time,
            success: true,
            error_message: None,
            used_replica: server_result.used_replica,
            used_cache: server_result.used_cache,
            cached_result: server_result.cached_result,
        }
    }
}

impl QueryExecutor {
    pub fn new() -> Self {
        Self {
            server: Arc::new(Mutex::new(None)),
        }
    }

    // Регистрируем запущенный сервер в executor
    pub async fn register_server(&self, server: SqlServer) {
        let mut server_guard = self.server.lock().await;
        *server_guard = Some(server);
        println!("[QueryExecutor] Server registered successfully");
    }

    pub async fn unregister_server(&self) {
        let mut server_guard = self.server.lock().await;
        *server_guard = None;
        println!("[QueryExecutor] Server unregistered");
    }

    pub async fn execute_query(&self, query: &str, client_ip: &str) -> QueryResult {
        let start_time = Instant::now();

        let server_guard = self.server.lock().await;
        let server = match server_guard.as_ref() {
            Some(server) => server,
            None => {
                return QueryResult {
                    rows_affected: 0,
                    execution_time: start_time.elapsed(),
                    success: false,
                    error_message: Some("SQL Server not registered".to_string()),
                    used_replica: false,
                    used_cache: false,
                    cached_result: None,
                };
            }
        };

        match server.execute_query(query, client_ip).await {
            Ok(server_result) => {
                let mut result: QueryResult = server_result.into();
                result.execution_time = start_time.elapsed();
                result
            },
            Err(e) => {
                let error_message = match e {
                    super::server::ServerError::NotRunning => "Server is not running".to_string(),
                    super::server::ServerError::SecurityError(sec_err) =>
                        format!("Security error: {}", sec_err),
                    super::server::ServerError::DatabaseError(db_err) =>
                        format!("Database error: {}", db_err),
                    _ => format!("Query execution error: {}", e),
                };

                QueryResult {
                    rows_affected: 0,
                    execution_time: start_time.elapsed(),
                    success: false,
                    error_message: Some(error_message),
                    used_replica: false,
                    used_cache: false,
                    cached_result: None,
                }
            },
        }
    }

    pub async fn is_server_registered(&self) -> bool {
        let server_guard = self.server.lock().await;
        server_guard.is_some()
    }

    pub async fn is_initialized(&self) -> bool {
        let server_guard = self.server.lock().await;
        server_guard.is_some()
    }

    pub async fn execute_batch(
        &self,
        queries: Vec<&str>,
        client_ip: &str,
    ) -> Result<Vec<QueryResult>, String> {
        let server_guard = self.server.lock().await;
        let server = match server_guard.as_ref() {
            Some(server) => server,
            None => return Err("SQL Server not registered".to_string()),
        };

        match server.execute_batch(queries, client_ip).await {
            Ok(server_results) => {
                let results: Vec<QueryResult> = server_results
                    .into_iter()
                    .map(|sr| sr.into())
                    .collect();
                Ok(results)
            },
            Err(e) => {
                Err(format!("Batch execution error: {}", e))
            },
        }
    }

    // ✅ ИСПРАВЛЕНО: Убрали доступ к приватному полю is_running
    pub async fn get_status(&self) -> ExecutorStatus {
        let server_guard = self.server.lock().await;
        // Вместо проверки is_running, проверяем можем ли мы выполнить health check
        let server_running = if let Some(server) = server_guard.as_ref() {
            // Пытаемся выполнить простую проверку здоровья
            match server.health_check().await.database {
                true => true,
                false => false,
            }
        } else {
            false
        };

        ExecutorStatus {
            server_registered: server_guard.is_some(),
            server_running,
        }
    }
}

pub struct ExecutorStatus {
    pub server_registered: bool,
    pub server_running: bool,
}

lazy_static::lazy_static! {
    pub static ref QUERY_EXECUTOR: QueryExecutor = QueryExecutor::new();
}