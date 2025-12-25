pub mod types;
pub mod traits;
pub mod controller;
pub mod instance;

// Реэкспорт основных типов и функций
pub use types::{
    Decision, PacketInfo, ConnectionMetrics, LoadLevel,
    ReputationScore, AnomalyScore, OffenseSeverity,
};
pub use controller::CongestionController;
pub use instance::{
    CONGESTION_CONTROLLER,
    CONGESTION_STATE
};

// Реэкспорт глобальных функций через явное указание
pub use instance::*;

// Объявляем подмодули
pub mod analyzer;
pub mod limiter;
pub mod reputation;
pub mod monitor;
pub mod auth;
pub mod config;
pub mod health_check;
pub mod validation;