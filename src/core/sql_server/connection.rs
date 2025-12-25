use sqlx::{PgPool, postgres::PgPoolOptions};
use std::time::Duration;
use tokio::sync::Semaphore;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::config::DatabaseConfig;

#[derive(Clone)]
pub struct HighPerformanceConnectionManager {
    primary_pool: PgPool,
    read_pool: PgPool,  // Отдельный пул для чтения
    write_pool: PgPool, // Отдельный пул для записи
    replica_pools: Vec<PgPool>,
    connection_semaphore: Arc<Semaphore>,
    max_connections: u32,
    current_connections: Arc<AtomicUsize>,
}

impl HighPerformanceConnectionManager {
    pub async fn new(config: DatabaseConfig) -> Result<Self, sqlx::Error> {
        config.validate().map_err(|e| {
            sqlx::Error::Configuration(e.to_string().into())
        })?;

        println!("[DB] Initializing HIGH-PERFORMANCE connection pools");

        // ОСНОВНОЙ ПУЛ (для записи и критичных операций)
        let primary_pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .acquire_timeout(Duration::from_secs(2)) // Быстрый timeout
            .idle_timeout(Duration::from_secs(60))
            .max_lifetime(Duration::from_secs(1800))
            .test_before_acquire(true)
            .connect(&config.primary_url)
            .await?;

        // ПУЛ ДЛЯ ЧТЕНИЯ (оптимизирован для SELECT)
        let read_pool = PgPoolOptions::new()
            .max_connections(config.max_connections * 2) // Больше соединений для чтения
            .min_connections(config.min_connections)
            .acquire_timeout(Duration::from_secs(1))
            .idle_timeout(Duration::from_secs(30))
            .max_lifetime(Duration::from_secs(3600))
            .connect(&config.primary_url)
            .await?;

        // ПУЛ ДЛЯ ЗАПИСИ (оптимизирован для INSERT/UPDATE/DELETE)
        let write_pool = PgPoolOptions::new()
            .max_connections(config.max_connections / 2) // Меньше соединений для записи
            .min_connections(5)
            .acquire_timeout(Duration::from_secs(3))
            .idle_timeout(Duration::from_secs(120))
            .max_lifetime(Duration::from_secs(1800))
            .connect(&config.primary_url)
            .await?;

        // ПУЛЫ РЕПЛИК
        let mut replica_pools = Vec::new();
        for replica_url in config.replica_urls {
            let replica_pool = PgPoolOptions::new()
                .max_connections(config.max_connections)
                .connect(&replica_url)
                .await?;
            replica_pools.push(replica_pool);
        }

        let max_connections = config.max_connections;
        let connection_semaphore = Arc::new(Semaphore::new(max_connections as usize));

        Ok(Self {
            primary_pool,
            read_pool,
            write_pool,
            replica_pools,
            connection_semaphore,
            max_connections,
            current_connections: Arc::new(AtomicUsize::new(0)),
        })
    }

    // ВЫБОР ПУЛА В ЗАВИСИМОСТИ ОТ ТИПА ЗАПРОСА
    pub fn get_pool_for_query(&self, is_read_query: bool) -> &PgPool {
        if is_read_query {
            self.get_read_pool()
        } else {
            self.get_write_pool()
        }
    }

    pub fn get_read_pool(&self) -> &PgPool {
        if self.replica_pools.is_empty() {
            &self.read_pool
        } else {
            // Round-robin между репликами
            static COUNTER: AtomicUsize = AtomicUsize::new(0);

            let index = COUNTER.fetch_add(1, Ordering::Relaxed) % self.replica_pools.len();
            &self.replica_pools[index]
        }
    }

    pub fn get_write_pool(&self) -> &PgPool {
        &self.write_pool
    }

    pub fn get_primary_pool(&self) -> &PgPool {
        &self.primary_pool
    }

    pub async fn acquire_connection(&self) -> Result<tokio::sync::SemaphorePermit<'_>, sqlx::Error> {
        let permit = self.connection_semaphore.acquire().await
            .map_err(|_| sqlx::Error::PoolClosed)?;

        self.current_connections.fetch_add(1, Ordering::SeqCst);
        Ok(permit)
    }

    pub fn release_connection(&self) {
        self.current_connections.fetch_sub(1, Ordering::SeqCst);
    }

    pub async fn health_check(&self) -> bool {
        // Проверяем все пулы
        let checks = vec![
            self.check_pool_health(&self.primary_pool, "primary").await,
            self.check_pool_health(&self.read_pool, "read").await,
            self.check_pool_health(&self.write_pool, "write").await,
        ];

        // ✅ ИСПРАВЛЕНО: Создаем строку заранее чтобы избежать временного значения
        for (i, replica_pool) in self.replica_pools.iter().enumerate() {
            let pool_name = format!("replica_{}", i); // Создаем здесь
            let replica_check = self.check_pool_health(replica_pool, &pool_name).await;
            if !replica_check {
                return false;
            }
        }

        for check in checks {
            if !check {
                return false;
            }
        }
        true
    }

    async fn check_pool_health(&self, pool: &PgPool, pool_name: &str) -> bool {
        match sqlx::query("SELECT 1").execute(pool).await {
            Ok(_) => true,
            Err(e) => {
                eprintln!("[DB Health Check] {} pool failed: {}", pool_name, e);
                false
            }
        }
    }

    pub fn get_connection_stats(&self) -> ConnectionStats {
        ConnectionStats {
            total_connections: self.max_connections,
            available_connections: self.connection_semaphore.available_permits() as u32,
            current_connections: self.current_connections.load(Ordering::Relaxed) as u32, // ✅ ИСПРАВЛЕНО
            replica_count: self.replica_pools.len() as u32,
            read_pool_size: self.read_pool.size(),
            write_pool_size: self.write_pool.size(),
        }
    }

    pub async fn adjust_pool_for_load(&self, current_load: f64) {
        // В реальной реализации здесь будет логика адаптации размеров пулов
        // на основе текущей нагрузки
        if current_load > 0.8 {
            // Логика для высокой нагрузки
            println!("[ConnectionManager] High load detected: {:.2}", current_load);
        }
    }
}

#[derive(Debug)]
pub struct ConnectionStats {
    pub total_connections: u32,
    pub available_connections: u32,
    pub current_connections: u32,
    pub replica_count: u32,
    pub read_pool_size: u32,
    pub write_pool_size: u32,
}