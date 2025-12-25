// Основные модули SQL Server
pub mod server;
pub mod executor;
pub mod connection;
pub mod config;
pub mod metrics;
pub mod monitoring;
pub mod health;
pub mod scaling;
pub mod security;
pub mod orm;
pub mod query;
pub mod init;
pub mod grade;
pub mod utils;

// Re-export часто используемых типов для удобства
pub use server::{SqlServer, QueryResult as ServerQueryResult, ServerError};
pub use executor::{QueryExecutor, QUERY_EXECUTOR, QueryResult, ExecutorStatus};
pub use connection::{HighPerformanceConnectionManager, ConnectionStats};
pub use config::{DatabaseConfig, SecurityConfig, ScalingConfig, AppConfig, ConfigError};
pub use metrics::{MetricsCollector, ServerMetrics, QueryMetrics};
pub use monitoring::{PerformanceMonitor, AlertSystem, RealTimeMetrics};
pub use health::checker::HealthChecker;
pub use scaling::balancer::{ReadReplicaBalancer, LoadBalancingStrategy};
pub use scaling::auto_scaler::{AutoScaler, AutoScalingConfig};
pub use security::advanced::AdvancedSecurityLayer;
pub use security::error::SecurityError;
pub use orm::cache::{MultiLevelCache, QueryResultCache};
pub use orm::instance::HighPerformanceORM;
pub use query::condition::Condition;
pub use query::types::{Join, Order, JoinType};
pub use query::parameter::QueryParameter;
pub use query::select::SelectBuilder;
pub use query::update::UpdateBuilder;
pub use query::insert::InsertBuilder;
pub use query::delete::DeleteBuilder;

// Инициализация
pub use init::{initialize_sql_server, shutdown_sql_server, restart_sql_server};

// Промышленный класс
pub use grade::IndustrialGradeDatabase;