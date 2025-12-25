use dashmap::DashMap;
use sqlx::{PgPool, Row, Column};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::time::{Duration, Instant};

use super::config::{DatabaseConfig, SecurityConfig};
use super::connection::HighPerformanceConnectionManager;
use super::security::advanced::AdvancedSecurityLayer;
use super::security::error::SecurityError;
use super::config::ConfigError;
use super::metrics::metrics::{MetricsCollector, ServerMetrics, QueryTypeStats};
use super::orm::cache::QueryResultCache;

#[derive(Debug)]
pub struct BatchQuery {
    pub query: String,
    pub client_ip: String,
    pub timestamp: Instant,
}

pub struct BatchProcessor {
    max_batch_size: usize,
    current_batch: Mutex<Vec<BatchQuery>>,
    processed_batches: std::sync::atomic::AtomicU64,
}

impl BatchProcessor {
    pub fn new(max_batch_size: usize) -> Self {
        Self {
            max_batch_size,
            current_batch: Mutex::new(Vec::new()),
            processed_batches: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub async fn add_query(self: &Arc<Self>, query: BatchQuery, sql_server: &SqlServer) -> Result<(), ServerError> {
        let mut batch = self.current_batch.lock().await;
        batch.push(query);

        if batch.len() >= self.max_batch_size {
            // ✅ ТЕПЕРЬ ИСПОЛЬЗУЕМ process_batch
            sql_server.process_batch(&mut batch).await?;
            self.processed_batches.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            batch.clear();
        }

        Ok(())
    }

    pub async fn force_process(self: &Arc<Self>, sql_server: &SqlServer) -> Result<(), ServerError> {
        let mut batch = self.current_batch.lock().await;
        if !batch.is_empty() {
            // ✅ ТЕПЕРЬ ИСПОЛЬЗУЕМ process_batch
            sql_server.process_batch(&mut batch).await?;
            self.processed_batches.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            batch.clear();
        }
        Ok(())
    }

    pub async fn get_current_batch_size(&self) -> usize {
        let batch = self.current_batch.lock().await;
        batch.len()
    }

    pub fn get_processed_batches_count(&self) -> u64 {
        self.processed_batches.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn get_max_batch_size(&self) -> usize {
        self.max_batch_size
    }
}

pub struct SqlServer {
    connection_manager: HighPerformanceConnectionManager,
    security_layer: AdvancedSecurityLayer,
    prepared_statements: Arc<DashMap<String, String>>,
    batch_processor: Arc<BatchProcessor>,
    metrics: Arc<MetricsCollector>,
    query_cache: Arc<QueryResultCache>,
    is_running: bool,
}

impl SqlServer {
    pub async fn new(db_config: DatabaseConfig, security_config: SecurityConfig) -> Result<Self, ServerError> {
        db_config.validate()?;
        security_config.validate()?;

        let connection_manager = HighPerformanceConnectionManager::new(db_config).await?;
        let security_layer = AdvancedSecurityLayer::new(security_config)?;
        let metrics = Arc::new(MetricsCollector::new());
        let query_cache = Arc::new(QueryResultCache::new(10000, Duration::from_secs(300)));
        let prepared_statements = Arc::new(DashMap::new());
        let batch_processor = Arc::new(BatchProcessor::new(100));

        Ok(Self {
            connection_manager,
            security_layer,
            prepared_statements,
            batch_processor,
            metrics,
            query_cache,
            is_running: false,
        })
    }

    pub async fn start(&mut self) -> Result<(), ServerError> {
        if self.is_running {
            return Err(ServerError::AlreadyRunning);
        }

        if !self.connection_manager.health_check().await {
            return Err(ServerError::DatabaseConnectionFailed);
        }

        self.is_running = true;
        println!("[SQL Server] Started successfully with prepared statements cache");
        Ok(())
    }

    pub async fn execute_query(
        &self,
        query: &str,
        client_ip: &str,
    ) -> Result<QueryResult, ServerError> {
        if !self.is_running {
            return Err(ServerError::NotRunning);
        }

        let timer = self.metrics.record_query_start()
            .set_query_type(self.detect_query_type(query))
            .set_client_ip(client_ip);

        // Проверка безопасности
        self.security_layer.validate_query(query, client_ip).await?;

        let is_read_query = query.trim_start().to_uppercase().starts_with("SELECT");
        let has_returning = query.to_uppercase().contains("RETURNING");

        // Проверяем кэш для SELECT запросов
        if is_read_query && !has_returning {
            if let Some(cached) = self.query_cache.get_query_result(query, &[]).await {
                self.metrics.record_cache_hit();

                let query_metrics = timer.finish(0, true, false);
                self.metrics.record_query_success(query_metrics);

                return Ok(QueryResult {
                    rows_affected: 0,
                    execution_time: Duration::default(),
                    used_replica: false,
                    used_cache: true,
                    cached_result: Some(cached),
                });
            }
        }

        self.metrics.record_cache_miss();

        let pool = self.connection_manager.get_pool_for_query(is_read_query);
        let statement_sql = self.get_prepared_statement(query).await?;

        let start_time = Instant::now();

        // ✅ ИСПРАВЛЕНИЕ: Обрабатываем запросы с RETURNING
        if is_read_query || has_returning {
            // Для SELECT запросов ИЛИ запросов с RETURNING получаем данные и конвертируем в JSON
            let result = sqlx::query(&statement_sql).fetch_all(pool).await?;
            let duration = start_time.elapsed();

            let json_result = Self::rows_to_json(result);

            // Кэшируем результат только для SELECT запросов без RETURNING
            if is_read_query && !has_returning {
                self.query_cache.set_query_result(query, &[], json_result.clone()).await;
            }

            let used_replica = is_read_query &&
                std::ptr::eq(pool as *const _, self.connection_manager.get_read_pool() as *const _);

            let query_metrics = timer.finish(
                0,
                false,
                used_replica
            );
            self.metrics.record_query_success(query_metrics);

            Ok(QueryResult {
                rows_affected: 0,
                execution_time: duration,
                used_replica,
                used_cache: false,
                cached_result: Some(json_result),
            })
        } else {
            // Для не-SELECT запросов без RETURNING (INSERT, UPDATE, DELETE)
            let result = sqlx::query(&statement_sql).execute(pool).await?;
            let duration = start_time.elapsed();

            let used_replica = is_read_query &&
                std::ptr::eq(pool as *const _, self.connection_manager.get_read_pool() as *const _);

            let query_metrics = timer.finish(
                result.rows_affected(),
                false,
                used_replica
            );
            self.metrics.record_query_success(query_metrics);

            Ok(QueryResult {
                rows_affected: result.rows_affected(),
                execution_time: duration,
                used_replica,
                used_cache: false,
                cached_result: None,
            })
        }
    }

    pub async fn execute_batch(
        &self,
        queries: Vec<&str>,
        client_ip: &str,
    ) -> Result<Vec<QueryResult>, ServerError> {
        if !self.is_running {
            return Err(ServerError::NotRunning);
        }

        let batch_timer = self.metrics.record_query_start()
            .set_query_type("BATCH")
            .set_client_ip(client_ip);

        let mut results = Vec::with_capacity(queries.len());
        let mut read_queries = Vec::new();
        let mut write_queries = Vec::new();

        for query in queries {
            self.security_layer.validate_query(query, client_ip).await?;

            let is_read = query.trim_start().to_uppercase().starts_with("SELECT");
            if is_read {
                read_queries.push(query);
            } else {
                write_queries.push(query);
            }
        }

        let (read_results, write_results) = tokio::join!(
            self.execute_read_batch(&read_queries, client_ip),
            self.execute_write_batch(&write_queries, client_ip)
        );

        results.extend(read_results?);
        results.extend(write_results?);

        self.metrics.record_batch_processed(results.len() as u64);

        let query_metrics = batch_timer.finish(
            results.iter().map(|r| r.rows_affected).sum(),
            false,
            false
        );
        self.metrics.record_query_success(query_metrics);

        Ok(results)
    }

    async fn execute_read_batch(
        &self,
        queries: &[&str],
        client_ip: &str,
    ) -> Result<Vec<QueryResult>, ServerError> {
        let pool = self.connection_manager.get_read_pool();
        self.execute_batch_on_pool(queries, pool, client_ip).await
    }

    async fn execute_write_batch(
        &self,
        queries: &[&str],
        client_ip: &str,
    ) -> Result<Vec<QueryResult>, ServerError> {
        let pool = self.connection_manager.get_write_pool();
        self.execute_batch_on_pool(queries, pool, client_ip).await
    }

    async fn execute_batch_on_pool(
        &self,
        queries: &[&str],
        pool: &PgPool,
        _client_ip: &str,
    ) -> Result<Vec<QueryResult>, ServerError> {
        let mut results = Vec::with_capacity(queries.len());

        for query in queries {
            let statement_sql = self.get_prepared_statement(query).await?;
            let start_time = Instant::now();
            let result = sqlx::query(&statement_sql).execute(pool).await?;
            let duration = start_time.elapsed();

            let used_replica = std::ptr::eq(pool as *const _, self.connection_manager.get_read_pool() as *const _);

            results.push(QueryResult {
                rows_affected: result.rows_affected(),
                execution_time: duration,
                used_replica,
                used_cache: false,
                cached_result: None,
            });
        }

        Ok(results)
    }

    // ✅ ДОПИСАННЫЙ МЕТОД process_batch
    async fn process_batch(&self, batch: &mut Vec<BatchQuery>) -> Result<(), ServerError> {
        if batch.is_empty() {
            return Ok(());
        }

        let start_time = Instant::now();
        let mut successful_queries = 0;
        let mut failed_queries = 0;

        // Группируем запросы по типу для оптимизации
        let mut read_queries = Vec::new();
        let mut write_queries = Vec::new();

        for batch_query in batch.iter() {
            let query = &batch_query.query;
            if query.trim_start().to_uppercase().starts_with("SELECT") {
                read_queries.push((query.as_str(), &batch_query.client_ip));
            } else {
                write_queries.push((query.as_str(), &batch_query.client_ip));
            }
        }

        // Выполняем READ запросы параллельно
        let read_futures: Vec<_> = read_queries.into_iter()
            .map(|(query, client_ip)| async {
                self.execute_single_query_in_batch(query, client_ip).await
            })
            .collect();

        let read_results = futures::future::join_all(read_futures).await;

        // Выполняем WRITE запросы последовательно (для консистентности)
        let mut write_results = Vec::new();
        for (query, client_ip) in write_queries {
            let result = self.execute_single_query_in_batch(query, client_ip).await;
            write_results.push(result);
        }

        // Собираем результаты
        for result in read_results.into_iter().chain(write_results) {
            match result {
                Ok(_) => successful_queries += 1,
                Err(_) => failed_queries += 1,
            }
        }

        let execution_time = start_time.elapsed();

        // Логируем результаты батч-обработки
        println!(
            "[SqlServer] Batch processed: {} queries, {} successful, {} failed, took {:?}",
            batch.len(),
            successful_queries,
            failed_queries,
            execution_time
        );

        // Обновляем метрики
        self.metrics.record_batch_processed(batch.len() as u64);

        if failed_queries > 0 {
            println!("[SqlServer] Batch processing completed with {} failures", failed_queries);
        }

        Ok(())
    }

    // ✅ ВСПОМОГАТЕЛЬНЫЙ МЕТОД для выполнения одного запроса в батче
    async fn execute_single_query_in_batch(&self, query: &str, client_ip: &str) -> Result<QueryResult, ServerError> {
        if !self.is_running {
            return Err(ServerError::NotRunning);
        }

        // Проверка безопасности
        self.security_layer.validate_query(query, client_ip).await?;

        let is_read_query = query.trim_start().to_uppercase().starts_with("SELECT");
        let has_returning = query.to_uppercase().contains("RETURNING");

        let pool = self.connection_manager.get_pool_for_query(is_read_query);
        let statement_sql = self.get_prepared_statement(query).await?;

        let start_time = Instant::now();

        // ✅ ИСПРАВЛЕНИЕ: Обрабатываем запросы с RETURNING в батчах
        if is_read_query || has_returning {
            let result = sqlx::query(&statement_sql).fetch_all(pool).await?;
            let duration = start_time.elapsed();
            let json_result = Self::rows_to_json(result);

            Ok(QueryResult {
                rows_affected: 0,
                execution_time: duration,
                used_replica: false,
                used_cache: false,
                cached_result: Some(json_result),
            })
        } else {
            let result = sqlx::query(&statement_sql).execute(pool).await?;
            let duration = start_time.elapsed();

            Ok(QueryResult {
                rows_affected: result.rows_affected(),
                execution_time: duration,
                used_replica: false,
                used_cache: false,
                cached_result: None,
            })
        }
    }

    // КЭШ PREPARED STATEMENTS
    async fn get_prepared_statement(
        &self,
        query: &str,
    ) -> Result<String, ServerError> {
        let query_hash = format!("{:x}", md5::compute(query));

        if let Some(statement_sql) = self.prepared_statements.get(&query_hash) {
            return Ok(statement_sql.clone());
        }

        // Просто сохраняем SQL, а не Statement
        self.prepared_statements.insert(query_hash.clone(), query.to_string());

        Ok(query.to_string())
    }

    // Максимально простой подход - все конвертируем в строки
    fn rows_to_json(rows: Vec<sqlx::postgres::PgRow>) -> String {
        let mut json_array = Vec::new();

        for row in rows {
            let mut json_obj = serde_json::Map::new();

            for column in row.columns() {
                let column_name = column.name();

                // Простой подход: все конвертируем в строку через to_string
                if let Ok(val) = row.try_get::<Option<String>, _>(column_name) {
                    if let Some(v) = val {
                        json_obj.insert(column_name.to_string(), Value::from(v));
                    } else {
                        json_obj.insert(column_name.to_string(), Value::Null);
                    }
                } else {
                    // Если не получается как String, пробуем другие базовые типы
                    if let Ok(val) = row.try_get::<Option<i64>, _>(column_name) {
                        if let Some(v) = val {
                            json_obj.insert(column_name.to_string(), Value::from(v));
                        } else {
                            json_obj.insert(column_name.to_string(), Value::Null);
                        }
                    } else if let Ok(val) = row.try_get::<Option<bool>, _>(column_name) {
                        if let Some(v) = val {
                            json_obj.insert(column_name.to_string(), Value::from(v));
                        } else {
                            json_obj.insert(column_name.to_string(), Value::Null);
                        }
                    } else {
                        json_obj.insert(column_name.to_string(), Value::Null);
                    }
                }
            }

            json_array.push(Value::Object(json_obj));
        }

        serde_json::to_string(&json_array).unwrap_or_else(|_| "[]".to_string())
    }

    pub async fn health_check(&self) -> ServerHealth {
        let db_healthy = self.connection_manager.health_check().await;
        let stats = self.connection_manager.get_connection_stats();

        ServerHealth {
            database: db_healthy,
            connection_stats: stats,
            uptime: Instant::now(),
            prepared_statements_count: self.prepared_statements.len(),
        }
    }

    pub async fn get_metrics(&self) -> ServerMetrics {
        self.metrics.get_metrics()
    }

    pub async fn get_query_stats(&self, query_type: &str) -> QueryTypeStats {
        self.metrics.get_query_stats(query_type)
    }

    fn detect_query_type(&self, query: &str) -> &str {
        let upper_query = query.trim_start().to_uppercase();
        if upper_query.starts_with("SELECT") { "SELECT" }
        else if upper_query.starts_with("INSERT") {
            if upper_query.contains("RETURNING") {
                "INSERT_RETURNING"
            } else {
                "INSERT"
            }
        }
        else if upper_query.starts_with("UPDATE") {
            if upper_query.contains("RETURNING") {
                "UPDATE_RETURNING"
            } else {
                "UPDATE"
            }
        }
        else if upper_query.starts_with("DELETE") {
            if upper_query.contains("RETURNING") {
                "DELETE_RETURNING"
            } else {
                "DELETE"
            }
        }
        else { "OTHER" }
    }

    // ✅ МЕТОДЫ ДЛЯ РАБОТЫ С BATCH_PROCESSOR
    pub async fn add_to_batch(&self, query: String, client_ip: String) -> Result<(), ServerError> {
        let batch_query = BatchQuery {
            query,
            client_ip,
            timestamp: Instant::now(),
        };

        // ✅ ПЕРЕДАЕМ self для вызова process_batch
        self.batch_processor.add_query(batch_query, self).await
    }


    pub async fn get_batch_stats(&self) -> BatchStats {
        let current_size = self.batch_processor.get_current_batch_size().await;
        let processed_count = self.batch_processor.get_processed_batches_count();
        let max_size = self.batch_processor.get_max_batch_size();

        BatchStats {
            current_batch_size: current_size,
            processed_batches_count: processed_count,
            max_batch_size: max_size,
        }
    }

    pub async fn flush_batch(&self) -> Result<(), ServerError> {
        // ✅ ПЕРЕДАЕМ self для вызова process_batch
        self.batch_processor.force_process(self).await
    }
}

#[derive(Debug)]
pub struct QueryResult {
    pub rows_affected: u64,
    pub execution_time: Duration,
    pub used_replica: bool,
    pub used_cache: bool,
    pub cached_result: Option<String>,
}

pub struct ServerHealth {
    pub database: bool,
    pub connection_stats: super::connection::ConnectionStats,
    pub uptime: Instant,
    pub prepared_statements_count: usize,
}

#[derive(Debug, Clone)]
pub struct BatchStats {
    pub current_batch_size: usize,
    pub processed_batches_count: u64,
    pub max_batch_size: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Database connection error: {0}")]
    DatabaseError(#[from] sqlx::Error),
    #[error("Security validation failed: {0}")]
    SecurityError(#[from] SecurityError),
    #[error("Configuration error: {0}")]
    Config(#[from] ConfigError),
    #[error("Server is not running")]
    NotRunning,
    #[error("Server is already running")]
    AlreadyRunning,
    #[error("Database connection failed")]
    DatabaseConnectionFailed,
    #[error("No replica database available")]
    NoReplicaAvailable,
    #[error("Batch processing error")]
    BatchError,
}