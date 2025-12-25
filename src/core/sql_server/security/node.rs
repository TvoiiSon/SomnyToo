use sqlx::{PgPool, postgres::PgPoolOptions, Error, Row};
use std::time::{Instant, Duration};
use tokio::sync::RwLock;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum NodeRole {
    Primary,
    Replica,
    Backup,
}

#[derive(Clone)]
pub struct DatabaseNode {
    pub url: String,
    pub pool: PgPool,
    pub role: NodeRole,
    pub weight: u32,
    pub region: String,
    pub last_health_check: Arc<RwLock<Instant>>, // ✅ ДОБАВЛЕНО: время последней проверки
}

#[derive(Clone, Default)]
pub struct NodeMetrics {
    pub connection_count: u32,
    pub query_count: u64,
    pub error_count: u64,
    pub avg_response_time: f64,
    pub last_health_check: Option<Instant>,
    pub is_healthy: bool,
    pub replication_lag: Option<Duration>, // ✅ ДОБАВЛЕНО: лаг репликации
}

impl DatabaseNode {
    pub async fn new(url: String, role: NodeRole) -> Result<Self, Error> {
        let pool = PgPoolOptions::new()
            .max_connections(20)
            .connect(&url)
            .await?;

        Ok(Self {
            url,
            pool,
            role,
            weight: 1,
            region: "default".to_string(),
            last_health_check: Arc::new(RwLock::new(Instant::now())),
        })
    }

    // ✅ ДОБАВЛЕНО: Проверка здоровья узла
    pub async fn health_check(&self) -> NodeHealth {
        let start_time = Instant::now();
        let is_healthy = match sqlx::query("SELECT 1").execute(&self.pool).await {
            Ok(_) => true,
            Err(_) => false,
        };
        let response_time = start_time.elapsed();

        // ✅ ДОБАВЛЕНО: Для реплик проверяем лаг репликации
        let replication_lag = if self.role == NodeRole::Replica {
            self.check_replication_lag().await.ok()
        } else {
            None
        };

        // Обновляем время последней проверки
        {
            let mut last_check = self.last_health_check.write().await;
            *last_check = Instant::now();
        }

        NodeHealth {
            is_healthy,
            response_time,
            replication_lag,
            last_checked: Instant::now(),
        }
    }

    // ✅ ДОБАВЛЕНО: Проверка лага репликации
    async fn check_replication_lag(&self) -> Result<Duration, Error> {
        // Для PostgreSQL проверяем лаг репликации
        if let Ok(row) = sqlx::query(
            "SELECT EXTRACT(EPOCH FROM (now() - pg_last_xact_replay_timestamp())) as lag_seconds"
        )
            .fetch_one(&self.pool)
            .await
        {
            let lag_seconds: Option<f64> = row.try_get("lag_seconds").ok();
            if let Some(seconds) = lag_seconds {
                return Ok(Duration::from_secs_f64(seconds));
            }
        }

        Ok(Duration::from_secs(0))
    }

    // ✅ ДОБАВЛЕНО: Получение информации об узле
    pub fn get_info(&self) -> NodeInfo {
        NodeInfo {
            url: self.url.clone(),
            role: self.role.clone(),
            weight: self.weight,
            region: self.region.clone(),
            pool_size: self.pool.size(),
        }
    }

    // ✅ ДОБАВЛЕНО: Проверка, нужно ли обновлять здоровье
    pub async fn should_check_health(&self) -> bool {
        let last_check = self.last_health_check.read().await;
        Instant::now().duration_since(*last_check) > Duration::from_secs(30)
    }

    // ✅ ДОБАВЛЕНО: Выполнение запроса с метриками
    pub async fn execute_query(&self, query: &str) -> Result<QueryResult, Error> {
        let start_time = Instant::now();

        let result = sqlx::query(query).execute(&self.pool).await;
        let execution_time = start_time.elapsed();

        match result {
            Ok(result) => Ok(QueryResult {
                success: true,
                rows_affected: result.rows_affected(),
                execution_time,
            }),
            Err(e) => {
                // Логируем ошибку
                eprintln!("[DatabaseNode] Query failed: {}", e);
                Err(e)
            }
        }
    }
}

// ✅ ДОБАВЛЕНО: Результат выполнения запроса
pub struct QueryResult {
    pub success: bool,
    pub rows_affected: u64,
    pub execution_time: Duration,
}

// ✅ ДОБАВЛЕНО: Информация о здоровье узла
pub struct NodeHealth {
    pub is_healthy: bool,
    pub response_time: Duration,
    pub replication_lag: Option<Duration>,
    pub last_checked: Instant,
}

// ✅ ДОБАВЛЕНО: Информация об узле
pub struct NodeInfo {
    pub url: String,
    pub role: NodeRole,
    pub weight: u32,
    pub region: String,
    pub pool_size: u32,
}