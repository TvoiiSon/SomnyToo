use crate::config::{AppConfig, DatabaseConfig as GlobalDatabaseConfig, SecurityConfig as GlobalSecurityConfig};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::time::Duration;

/// Конфигурация базы данных для SQL сервера
/// Теперь использует общую конфигурацию приложения
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DatabaseConfig {
    pub primary_url: String,
    pub replica_urls: Vec<String>,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connection_timeout: u64,
    pub acquire_timeout: u64,
    pub idle_timeout: u64,
    pub max_lifetime: u64,
    pub statement_cache_size: usize,
    pub connect_retries: u32,
}

impl DatabaseConfig {
    /// Создает конфигурацию из общей конфигурации приложения
    pub fn from_global_config(global_config: &GlobalDatabaseConfig) -> Self {
        Self {
            primary_url: global_config.primary_url.clone(),
            replica_urls: global_config.replica_urls.clone(),
            max_connections: global_config.max_connections,
            min_connections: global_config.min_connections,
            connection_timeout: global_config.connection_timeout,
            acquire_timeout: global_config.acquire_timeout,
            idle_timeout: global_config.idle_timeout,
            max_lifetime: global_config.max_lifetime,
            statement_cache_size: global_config.statement_cache_size,
            connect_retries: global_config.connect_retries,
        }
    }

    /// Создает конфигурацию из общего AppConfig
    pub fn from_app_config(app_config: &AppConfig) -> Self {
        Self::from_global_config(&app_config.database)
    }

    /// Конфигурация по умолчанию (использует общую конфигурацию)
    pub fn default_from_env() -> Self {
        let global_config = GlobalDatabaseConfig::from_env();
        Self::from_global_config(&global_config)
    }

    /// Валидация конфигурации
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_connections < self.min_connections {
            return Err(ConfigError::InvalidConnectionPoolSize);
        }

        if self.primary_url.is_empty() {
            return Err(ConfigError::MissingPrimaryUrl);
        }

        if !self.primary_url.starts_with("postgres://") &&
            !self.primary_url.starts_with("postgresql://") {
            return Err(ConfigError::InvalidUrlFormat);
        }

        if self.connection_timeout == 0 {
            return Err(ConfigError::InvalidTimeout("connection_timeout cannot be zero".to_string()));
        }

        if self.acquire_timeout == 0 {
            return Err(ConfigError::InvalidTimeout("acquire_timeout cannot be zero".to_string()));
        }

        if self.max_connections > 1000 {
            return Err(ConfigError::PoolSizeTooLarge);
        }

        if self.statement_cache_size > 10000 {
            return Err(ConfigError::CacheSizeTooLarge);
        }

        Ok(())
    }

    /// Получение таймаутов как Duration
    pub fn get_connection_timeout(&self) -> Duration {
        Duration::from_secs(self.connection_timeout)
    }

    pub fn get_acquire_timeout(&self) -> Duration {
        Duration::from_secs(self.acquire_timeout)
    }

    pub fn get_idle_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_timeout)
    }

    pub fn get_max_lifetime(&self) -> Duration {
        Duration::from_secs(self.max_lifetime)
    }

    /// Тестовая конфигурация
    pub fn test_config() -> Self {
        Self {
            primary_url: "postgres://test:test@localhost/test".to_string(),
            replica_urls: Vec::new(),
            max_connections: 10,
            min_connections: 2,
            connection_timeout: 30,
            acquire_timeout: 5,
            idle_timeout: 300,
            max_lifetime: 1800,
            statement_cache_size: 100,
            connect_retries: 1,
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self::default_from_env()
    }
}

/// Конфигурация безопасности для SQL сервера
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SecurityConfig {
    pub max_requests_per_minute: usize,
    pub query_timeout_ms: u64,
    pub max_query_length: usize,
    pub allowed_tables: Vec<String>,
    pub blocked_ips: Vec<IpAddr>,
    pub enable_sql_injection_protection: bool,
    pub enable_rate_limiting: bool,
    pub hmac_secret_key: String,
    pub aes_secret_key: String,
    pub psk_secret: String,
}

impl SecurityConfig {
    /// Создает конфигурацию из общей конфигурации приложения
    pub fn from_global_config(global_config: &GlobalSecurityConfig) -> Self {
        Self {
            max_requests_per_minute: global_config.max_requests_per_minute,
            query_timeout_ms: global_config.query_timeout_ms,
            max_query_length: global_config.max_query_length,
            allowed_tables: global_config.allowed_tables.clone(),
            blocked_ips: global_config.blocked_ips.clone(),
            enable_sql_injection_protection: global_config.enable_sql_injection_protection,
            enable_rate_limiting: global_config.enable_rate_limiting,
            hmac_secret_key: global_config.hmac_secret_key.clone(),
            aes_secret_key: global_config.aes_secret_key.clone(),
            psk_secret: global_config.psk_secret.clone(),
        }
    }

    /// Создает конфигурацию из общего AppConfig
    pub fn from_app_config(app_config: &AppConfig) -> Self {
        Self::from_global_config(&app_config.security)
    }

    /// Конфигурация по умолчанию (использует общую конфигурацию)
    pub fn default_from_env() -> Self {
        let global_config = GlobalSecurityConfig::from_env();
        Self::from_global_config(&global_config)
    }

    /// Валидация конфигурации
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_query_length > 10 * 1024 * 1024 {
            return Err(ConfigError::QueryTooLongLimit);
        }

        if self.query_timeout_ms == 0 {
            return Err(ConfigError::InvalidTimeout("query_timeout_ms cannot be zero".to_string()));
        }

        if self.max_requests_per_minute > 1_000_000 {
            return Err(ConfigError::RateLimitTooHigh);
        }

        if self.hmac_secret_key.is_empty() {
            return Err(ConfigError::MissingSecretKey("HMAC_SECRET_KEY".to_string()));
        }

        if self.aes_secret_key.is_empty() {
            return Err(ConfigError::MissingSecretKey("AES_SECRET_KEY".to_string()));
        }

        if self.psk_secret.is_empty() {
            return Err(ConfigError::MissingSecretKey("PSK_SECRET".to_string()));
        }

        Ok(())
    }

    /// Получение таймаута запроса как Duration
    pub fn get_query_timeout(&self) -> Duration {
        Duration::from_millis(self.query_timeout_ms)
    }

    /// Проверка разрешенности таблицы
    pub fn is_table_allowed(&self, table: &str) -> bool {
        self.allowed_tables.is_empty() || self.allowed_tables.iter().any(|t| t == table)
    }

    /// Проверка блокировки IP
    pub fn is_ip_blocked(&self, ip: &IpAddr) -> bool {
        self.blocked_ips.contains(ip)
    }

    /// Тестовая конфигурация
    pub fn test_config() -> Self {
        Self {
            max_requests_per_minute: 10_000,
            query_timeout_ms: 1000,
            max_query_length: 1024 * 64,
            allowed_tables: Vec::new(),
            blocked_ips: Vec::new(),
            enable_sql_injection_protection: false,
            enable_rate_limiting: false,
            hmac_secret_key: "test_hmac_key".to_string(),
            aes_secret_key: "test_aes_key".to_string(),
            psk_secret: "test_psk".to_string(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self::default_from_env()
    }
}

/// Конфигурация кэширования для SQL сервера
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CacheConfig {
    pub enable_query_cache: bool,
    pub query_cache_size: usize,
    pub query_cache_ttl_seconds: u64,
    pub enable_prepared_statements: bool,
    pub prepared_statements_cache_size: usize,
}

impl CacheConfig {
    /// Создает конфигурацию из общей конфигурации приложения
    pub fn from_app_config(app_config: &AppConfig) -> Self {
        Self {
            enable_query_cache: app_config.cache.enable_query_cache,
            query_cache_size: app_config.cache.query_cache_size,
            query_cache_ttl_seconds: app_config.cache.query_cache_ttl_seconds,
            enable_prepared_statements: app_config.cache.enable_prepared_statements,
            prepared_statements_cache_size: app_config.cache.prepared_statements_cache_size,
        }
    }

    /// Конфигурация по умолчанию (использует общую конфигурацию)
    pub fn default_from_env() -> Self {
        let app_config = AppConfig::from_env();
        Self::from_app_config(&app_config)
    }

    /// Валидация конфигурации
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.query_cache_size > 100_000 {
            return Err(ConfigError::CacheSizeTooLarge);
        }

        if self.prepared_statements_cache_size > 10_000 {
            return Err(ConfigError::CacheSizeTooLarge);
        }

        Ok(())
    }

    /// Получение TTL кэша запросов как Duration
    pub fn get_query_cache_ttl(&self) -> Duration {
        Duration::from_secs(self.query_cache_ttl_seconds)
    }

    /// Тестовая конфигурация
    pub fn test_config() -> Self {
        Self {
            enable_query_cache: false,
            query_cache_size: 0,
            query_cache_ttl_seconds: 0,
            enable_prepared_statements: true,
            prepared_statements_cache_size: 100,
        }
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self::default_from_env()
    }
}

/// Полная конфигурация SQL сервера
#[derive(Debug, Clone)]
pub struct SqlServerConfig {
    pub database: DatabaseConfig,
    pub security: SecurityConfig,
    pub cache: CacheConfig,
    pub log_level: String,
}

impl SqlServerConfig {
    /// Создает конфигурацию из общего AppConfig
    pub fn from_app_config(app_config: &AppConfig) -> Self {
        Self {
            database: DatabaseConfig::from_app_config(app_config),
            security: SecurityConfig::from_app_config(app_config),
            cache: CacheConfig::from_app_config(app_config),
            log_level: app_config.log_level.clone(),
        }
    }

    /// Конфигурация по умолчанию (использует общую конфигурацию)
    pub fn default_from_env() -> Self {
        let app_config = AppConfig::from_env();
        Self::from_app_config(&app_config)
    }

    /// Валидация всей конфигурации
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.database.validate()?;
        self.security.validate()?;
        self.cache.validate()?;

        let valid_log_levels = ["error", "warn", "info", "debug", "trace"];
        if !valid_log_levels.contains(&self.log_level.as_str()) {
            return Err(ConfigError::InvalidLogLevel(self.log_level.clone()));
        }

        Ok(())
    }

    /// Тестовая конфигурация
    pub fn test_config() -> Self {
        Self {
            database: DatabaseConfig::test_config(),
            security: SecurityConfig::test_config(),
            cache: CacheConfig::test_config(),
            log_level: "error".to_string(),
        }
    }
}

impl Default for SqlServerConfig {
    fn default() -> Self {
        Self::default_from_env()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Invalid connection pool size")]
    InvalidConnectionPoolSize,
    #[error("Missing primary database URL")]
    MissingPrimaryUrl,
    #[error("Invalid database URL format")]
    InvalidUrlFormat,
    #[error("Query length limit too high")]
    QueryTooLongLimit,
    #[error("Invalid timeout: {0}")]
    InvalidTimeout(String),
    #[error("Pool size too large")]
    PoolSizeTooLarge,
    #[error("Cache size too large")]
    CacheSizeTooLarge,
    #[error("Rate limit too high")]
    RateLimitTooHigh,
    #[error("Missing secret key: {0}")]
    MissingSecretKey(String),
    #[error("Invalid log level: {0}")]
    InvalidLogLevel(String),
}