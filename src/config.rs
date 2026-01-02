use dotenv::dotenv;
use serde::{Deserialize, Serialize};
use std::env;
use std::net::IpAddr;
use std::time::Duration;

/// Конфигурация фантомной криптосистемы
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PhantomConfig {
    pub enabled: bool,
    pub session_timeout_ms: u64,
    pub max_sessions: usize,
    pub enable_hardware_acceleration: bool,
    pub constant_time_enforced: bool,
    pub hardware_auth_enabled: bool,
    pub hardware_secret_key: String,
    pub enable_emulation_detection: bool,
    pub min_session_lifetime_ms: u64,
    pub max_operations_per_session: u64,
    pub assembler_type: String, // "auto", "avx2", "neon", "generic"
}

impl Default for PhantomConfig {
    fn default() -> Self {
        dotenv().ok();

        Self {
            enabled: env::var("PHANTOM_ENABLED")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(true),
            session_timeout_ms: env::var("PHANTOM_SESSION_TIMEOUT_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(90_000),
            max_sessions: env::var("PHANTOM_MAX_SESSIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(100_000),
            enable_hardware_acceleration: env::var("PHANTOM_HW_ACCELERATION")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(true),
            constant_time_enforced: env::var("PHANTOM_CONSTANT_TIME")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(true),
            hardware_auth_enabled: env::var("HARDWARE_AUTH_ENABLED")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(false),
            hardware_secret_key: env::var("HARDWARE_SECRET_KEY")
                .unwrap_or_else(|_| "".to_string()),
            enable_emulation_detection: env::var("ENABLE_EMULATION_DETECTION")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(true),
            min_session_lifetime_ms: env::var("PHANTOM_MIN_SESSION_LIFETIME_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10_000),
            max_operations_per_session: env::var("PHANTOM_MAX_OPERATIONS_PER_SESSION")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1_000_000),
            assembler_type: env::var("PHANTOM_ASSEMBLER_TYPE")
                .unwrap_or_else(|_| "auto".to_string()),
        }
    }
}

impl PhantomConfig {
    pub fn from_env() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.session_timeout_ms < self.min_session_lifetime_ms {
            return Err(ConfigError::InvalidPhantomConfig(
                format!("session_timeout_ms ({}) must be greater than min_session_lifetime_ms ({})",
                        self.session_timeout_ms, self.min_session_lifetime_ms)
            ));
        }

        if self.max_sessions > 1_000_000 {
            return Err(ConfigError::InvalidPhantomConfig(
                "max_sessions cannot exceed 1,000,000".to_string()
            ));
        }

        if self.session_timeout_ms == 0 {
            return Err(ConfigError::InvalidPhantomConfig(
                "session_timeout_ms cannot be zero".to_string()
            ));
        }

        if self.hardware_auth_enabled && self.hardware_secret_key.is_empty() {
            return Err(ConfigError::MissingSecretKey(
                "HARDWARE_SECRET_KEY must be set when hardware_auth_enabled is true".to_string()
            ));
        }

        let valid_assembler_types = ["auto", "avx2", "neon", "generic"];
        if !valid_assembler_types.contains(&self.assembler_type.as_str()) {
            return Err(ConfigError::InvalidPhantomConfig(
                format!("assembler_type must be one of: {:?}", valid_assembler_types)
            ));
        }

        if self.min_session_lifetime_ms < 1000 {
            return Err(ConfigError::InvalidPhantomConfig(
                "min_session_lifetime_ms must be at least 1000ms".to_string()
            ));
        }

        Ok(())
    }

    pub fn get_session_timeout(&self) -> Duration {
        Duration::from_millis(self.session_timeout_ms)
    }

    pub fn get_min_session_lifetime(&self) -> Duration {
        Duration::from_millis(self.min_session_lifetime_ms)
    }

    pub fn should_use_hardware_auth(&self) -> bool {
        self.enabled && self.hardware_auth_enabled
    }

    pub fn get_assembler_type(&self) -> &str {
        &self.assembler_type
    }

    pub fn test_config() -> Self {
        Self {
            enabled: true,
            session_timeout_ms: 30_000,
            max_sessions: 10_000,
            enable_hardware_acceleration: true,
            constant_time_enforced: true,
            hardware_auth_enabled: false,
            hardware_secret_key: "".to_string(),
            enable_emulation_detection: true,
            min_session_lifetime_ms: 5_000,
            max_operations_per_session: 100_000,
            assembler_type: "auto".to_string(),
        }
    }
}

/// Структура конфигурации сервера
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub hmac_secret_key: String,
    pub aes_secret_key: String,
    pub psk_secret: String,
}

impl ServerConfig {
    /// Загружает конфигурацию из .env файла или переменных окружения
    pub fn from_env() -> Self {
        // Загружаем .env файл (если он есть)
        dotenv().ok();

        // Получаем переменные окружения
        let host = env::var("SERVER_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port_str = env::var("SERVER_PORT").unwrap_or_else(|_| "8000".to_string());
        let port = port_str.parse::<u16>().unwrap_or(8000);

        let hmac_secret_key = env::var("HMAC_SECRET_KEY")
            .expect("Переменная HMAC_SECRET_KEY не установлена в .env файле");
        let aes_secret_key = env::var("AES_SECRET_KEY")
            .expect("Переменная AES_SECRET_KEY не установлена в .env файле");
        let psk_secret = env::var("PSK_SECRET")
            .expect("Переменная PSK_SECRET не установлена в .env файле");

        ServerConfig {
            host,
            port,
            hmac_secret_key,
            aes_secret_key,
            psk_secret,
        }
    }

    /// Возвращает строку адреса в виде "0.0.0.0:8000"
    pub fn get_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

pub fn database_url() -> String {
    dotenv().ok();
    env::var("DATABASE_URL").expect("DATABASE_URL must be set in .env file")
}

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

impl Default for DatabaseConfig {
    fn default() -> Self {
        // Используем DATABASE_URL из .env файла
        let db_url = database_url();

        Self {
            primary_url: db_url,
            replica_urls: Vec::new(),
            max_connections: env::var("DB_MAX_CONNECTIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(50),
            min_connections: env::var("DB_MIN_CONNECTIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
            connection_timeout: env::var("DB_CONNECTION_TIMEOUT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            acquire_timeout: env::var("DB_ACQUIRE_TIMEOUT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            idle_timeout: env::var("DB_IDLE_TIMEOUT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            max_lifetime: env::var("DB_MAX_LIFETIME")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1800),
            statement_cache_size: env::var("DB_STATEMENT_CACHE_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1000),
            connect_retries: env::var("DB_CONNECT_RETRIES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
        }
    }
}

impl DatabaseConfig {
    pub fn from_env() -> Self {
        Self::default()
    }

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

    pub fn test_config() -> Self {
        Self {
            primary_url: "postgres://test:test@localhost/test".to_string(),
            max_connections: 10,
            min_connections: 2,
            connect_retries: 1,
            ..Default::default()
        }
    }
}

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

impl Default for SecurityConfig {
    fn default() -> Self {
        dotenv().ok();

        Self {
            max_requests_per_minute: env::var("MAX_REQUESTS_PER_MINUTE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1000),
            query_timeout_ms: env::var("QUERY_TIMEOUT_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5000),
            max_query_length: env::var("MAX_QUERY_LENGTH")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1024 * 1024),
            allowed_tables: Vec::new(),
            blocked_ips: Vec::new(),
            enable_sql_injection_protection: env::var("ENABLE_SQL_INJECTION_PROTECTION")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(true),
            enable_rate_limiting: env::var("ENABLE_RATE_LIMITING")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(true),
            hmac_secret_key: env::var("HMAC_SECRET_KEY")
                .expect("HMAC_SECRET_KEY must be set in .env file"),
            aes_secret_key: env::var("AES_SECRET_KEY")
                .expect("AES_SECRET_KEY must be set in .env file"),
            psk_secret: env::var("PSK_SECRET")
                .expect("PSK_SECRET must be set in .env file"),
        }
    }
}

impl SecurityConfig {
    pub fn from_env() -> Self {
        Self::default()
    }

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

    pub fn get_query_timeout(&self) -> Duration {
        Duration::from_millis(self.query_timeout_ms)
    }

    pub fn is_table_allowed(&self, table: &str) -> bool {
        self.allowed_tables.is_empty() || self.allowed_tables.iter().any(|t| t == table)
    }

    pub fn is_ip_blocked(&self, ip: &IpAddr) -> bool {
        self.blocked_ips.contains(ip)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ScalingConfig {
    pub enable_auto_scaling: bool,
    pub max_replicas: usize,
    pub scale_up_cpu_threshold: f64,
    pub scale_down_cpu_threshold: f64,
    pub scale_up_connections_threshold: usize,
    pub scale_check_interval_seconds: u64,
    pub min_replicas: usize,
}

impl Default for ScalingConfig {
    fn default() -> Self {
        dotenv().ok();

        Self {
            enable_auto_scaling: env::var("ENABLE_AUTO_SCALING")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(true),
            max_replicas: env::var("MAX_REPLICAS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
            scale_up_cpu_threshold: env::var("SCALE_UP_CPU_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(80.0),
            scale_down_cpu_threshold: env::var("SCALE_DOWN_CPU_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30.0),
            scale_up_connections_threshold: env::var("SCALE_UP_CONNECTIONS_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1000),
            scale_check_interval_seconds: env::var("SCALE_CHECK_INTERVAL_SECONDS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60),
            min_replicas: env::var("MIN_REPLICAS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1),
        }
    }
}

impl ScalingConfig {
    pub fn from_env() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_replicas < self.min_replicas {
            return Err(ConfigError::InvalidScalingConfig(
                "max_replicas cannot be less than min_replicas".to_string()
            ));
        }

        if self.scale_up_cpu_threshold <= self.scale_down_cpu_threshold {
            return Err(ConfigError::InvalidScalingConfig(
                "scale_up_cpu_threshold must be greater than scale_down_cpu_threshold".to_string()
            ));
        }

        if self.scale_up_cpu_threshold > 100.0 || self.scale_down_cpu_threshold < 0.0 {
            return Err(ConfigError::InvalidScalingConfig(
                "CPU thresholds must be between 0 and 100".to_string()
            ));
        }

        if self.scale_check_interval_seconds == 0 {
            return Err(ConfigError::InvalidScalingConfig(
                "scale_check_interval_seconds cannot be zero".to_string()
            ));
        }

        Ok(())
    }

    pub fn get_scale_check_interval(&self) -> Duration {
        Duration::from_secs(self.scale_check_interval_seconds)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CacheConfig {
    pub enable_query_cache: bool,
    pub query_cache_size: usize,
    pub query_cache_ttl_seconds: u64,
    pub enable_prepared_statements: bool,
    pub prepared_statements_cache_size: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        dotenv().ok();

        Self {
            enable_query_cache: env::var("ENABLE_QUERY_CACHE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(true),
            query_cache_size: env::var("QUERY_CACHE_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10000),
            query_cache_ttl_seconds: env::var("QUERY_CACHE_TTL_SECONDS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            enable_prepared_statements: env::var("ENABLE_PREPARED_STATEMENTS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(true),
            prepared_statements_cache_size: env::var("PREPARED_STATEMENTS_CACHE_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1000),
        }
    }
}

impl CacheConfig {
    pub fn from_env() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.query_cache_size > 100_000 {
            return Err(ConfigError::CacheSizeTooLarge);
        }

        if self.prepared_statements_cache_size > 10_000 {
            return Err(ConfigError::CacheSizeTooLarge);
        }

        Ok(())
    }

    pub fn get_query_cache_ttl(&self) -> Duration {
        Duration::from_secs(self.query_cache_ttl_seconds)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AppConfig {
    pub database: DatabaseConfig,
    pub security: SecurityConfig,
    pub scaling: ScalingConfig,
    pub cache: CacheConfig,
    pub phantom: PhantomConfig,
    pub log_level: String,
    pub server: ServerConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        dotenv().ok();

        Self {
            database: DatabaseConfig::from_env(),
            security: SecurityConfig::from_env(),
            scaling: ScalingConfig::from_env(),
            cache: CacheConfig::from_env(),
            phantom: PhantomConfig::from_env(),
            log_level: env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string()),
            server: ServerConfig::from_env(),
        }
    }
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.database.validate()?;
        self.security.validate()?;
        self.scaling.validate()?;
        self.cache.validate()?;
        self.phantom.validate()?;

        let valid_log_levels = ["error", "warn", "info", "debug", "trace"];
        if !valid_log_levels.contains(&self.log_level.as_str()) {
            return Err(ConfigError::InvalidLogLevel(self.log_level.clone()));
        }

        Ok(())
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
    #[error("Invalid scaling configuration: {0}")]
    InvalidScalingConfig(String),
    #[error("Invalid log level: {0}")]
    InvalidLogLevel(String),
    #[error("Missing secret key: {0}")]
    MissingSecretKey(String),
    #[error("Invalid phantom configuration: {0}")]
    InvalidPhantomConfig(String),
}