use super::config::DatabaseConfig;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct IndustrialGradeDatabase {
    config: DatabaseConfig,
    metrics: Arc<RwLock<DatabaseMetrics>>,
    health_checker: Arc<HealthChecker>,
}

#[derive(Debug, Clone)]
pub struct DatabaseMetrics {
    pub total_read_operations: u64,
    pub total_write_operations: u64,
    pub successful_operations: u64,
    pub failed_operations: u64,
    pub average_read_time: Duration,
    pub average_write_time: Duration,
    pub last_operation_time: Option<Instant>,
}

impl Default for DatabaseMetrics {
    fn default() -> Self {
        Self {
            total_read_operations: 0,
            total_write_operations: 0,
            successful_operations: 0,
            failed_operations: 0,
            average_read_time: Duration::default(),
            average_write_time: Duration::default(),
            last_operation_time: None,
        }
    }
}

#[derive(Clone)]
pub struct HealthChecker {
    last_health_check: Arc<RwLock<Instant>>,
    is_healthy: Arc<RwLock<bool>>,
}

impl IndustrialGradeDatabase {
    pub async fn new(config: DatabaseConfig) -> Result<Self, Box<dyn std::error::Error>> {
        config.validate()?;

        let database = IndustrialGradeDatabase {
            config,
            metrics: Arc::new(RwLock::new(DatabaseMetrics::default())),
            health_checker: Arc::new(HealthChecker {
                last_health_check: Arc::new(RwLock::new(Instant::now())),
                is_healthy: Arc::new(RwLock::new(true)),
            }),
        };

        database.start_background_tasks().await;

        Ok(database)
    }

    pub async fn execute_read<F, T>(&self, operation: F) -> Result<T, Box<dyn std::error::Error>>
    where
        F: FnOnce() -> T,
    {
        let start_time = Instant::now();

        if !self.is_healthy().await {
            return Err("Database is not healthy".into());
        }

        let result = operation();

        self.record_read_operation(start_time.elapsed(), true).await;

        Ok(result)
    }

    pub async fn execute_write<F, T>(&self, operation: F) -> Result<T, Box<dyn std::error::Error>>
    where
        F: FnOnce() -> T,
    {
        let start_time = Instant::now();

        if !self.is_healthy().await {
            return Err("Database is not healthy".into());
        }

        let result = operation();

        self.record_write_operation(start_time.elapsed(), true).await;

        Ok(result)
    }

    // ✅ ИСПРАВЛЕНО: Убрали неоднозначность типа
    pub async fn execute_with_timeout<F, T>(
        &self,
        operation: F,
        timeout: Duration
    ) -> Result<T, Box<dyn std::error::Error>>
    where
        F: FnOnce() -> T,
    {
        tokio::time::timeout(timeout, async {
            self.execute_read(operation).await
        }).await.unwrap_or_else(|_| Err("Operation timeout".into()))
    }

    pub async fn get_metrics(&self) -> DatabaseMetrics {
        let metrics = self.metrics.read().await;
        metrics.clone()
    }

    pub async fn is_healthy(&self) -> bool {
        let is_healthy = self.health_checker.is_healthy.read().await;
        *is_healthy
    }

    pub async fn health_check(&self) -> bool {
        let is_healthy = true;

        {
            let mut health_guard = self.health_checker.is_healthy.write().await;
            *health_guard = is_healthy;
        }

        {
            let mut last_check = self.health_checker.last_health_check.write().await;
            *last_check = Instant::now();
        }

        is_healthy
    }

    pub fn get_config(&self) -> &DatabaseConfig {
        &self.config
    }

    async fn start_background_tasks(&self) {
        let health_checker = self.health_checker.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                let _ = HealthChecker::perform_health_check(&health_checker).await;
            }
        });
    }

    async fn record_read_operation(&self, duration: Duration, success: bool) {
        let mut metrics = self.metrics.write().await;
        metrics.total_read_operations += 1;
        if success {
            metrics.successful_operations += 1;
        } else {
            metrics.failed_operations += 1;
        }

        let total_time = metrics.average_read_time * metrics.total_read_operations as u32 + duration;
        metrics.average_read_time = total_time / (metrics.total_read_operations as u32 + 1);
        metrics.last_operation_time = Some(Instant::now());
    }

    async fn record_write_operation(&self, duration: Duration, success: bool) {
        let mut metrics = self.metrics.write().await;
        metrics.total_write_operations += 1;
        if success {
            metrics.successful_operations += 1;
        } else {
            metrics.failed_operations += 1;
        }

        let total_time = metrics.average_write_time * metrics.total_write_operations as u32 + duration;
        metrics.average_write_time = total_time / (metrics.total_write_operations as u32 + 1);
        metrics.last_operation_time = Some(Instant::now());
    }
}

impl HealthChecker {
    async fn perform_health_check(health_checker: &HealthChecker) -> bool {
        let is_healthy = true;

        {
            let mut health_guard = health_checker.is_healthy.write().await;
            *health_guard = is_healthy;
        }

        {
            let mut last_check = health_checker.last_health_check.write().await;
            *last_check = Instant::now();
        }

        is_healthy
    }
}

pub struct IndustrialGradeDatabaseBuilder {
    config: Option<DatabaseConfig>,
}

impl IndustrialGradeDatabaseBuilder {
    pub fn new() -> Self {
        Self { config: None }
    }

    pub fn with_config(mut self, config: DatabaseConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn with_primary_url(mut self, url: String) -> Self {
        let mut config = self.config.unwrap_or_default();
        config.primary_url = url;
        self.config = Some(config);
        self
    }

    pub fn with_max_connections(mut self, max_connections: u32) -> Self {
        let mut config = self.config.unwrap_or_default();
        config.max_connections = max_connections;
        self.config = Some(config);
        self
    }

    pub async fn build(self) -> Result<IndustrialGradeDatabase, Box<dyn std::error::Error>> {
        let config = self.config.unwrap_or_default();
        IndustrialGradeDatabase::new(config).await
    }
}

impl Default for IndustrialGradeDatabaseBuilder {
    fn default() -> Self {
        Self::new()
    }
}