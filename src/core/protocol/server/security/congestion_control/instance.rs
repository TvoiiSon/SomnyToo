use std::sync::Arc;
use std::net::IpAddr;
use std::time::Duration;
use once_cell::sync::Lazy;
use tokio::sync::RwLock;

use super::controller::{CongestionController, ControllerMonitor};
use super::traits::{TrafficAnalyzer, AdaptiveLimiter, ReputationManager, CongestionMonitor};
use super::types::{
    Decision, ConnectionMetrics
};
use crate::core::protocol::server::security::rate_limiter::classifier::PacketPriority;
use super::analyzer::traffic_analyzer::TrafficAnalyzerImpl;
use super::limiter::adaptive_limiter::AdaptiveLimiterImpl;
use super::reputation::reputation_manager::ReputationManagerImpl;
use super::auth::AuthManager;
use super::config::ConfigManager;

// Добавляем недостающий тип ControllerState
#[derive(Debug, Clone)]
pub struct ControllerState {
    pub is_enabled: bool,
    pub learning_mode: bool,
    pub last_update: std::time::Instant,
}

impl Default for ControllerState {
    fn default() -> Self {
        Self {
            is_enabled: true,
            learning_mode: true,
            last_update: std::time::Instant::now(),
        }
    }
}

// Создаем AuthManager для инициализации
fn create_auth_manager() -> Arc<AuthManager> {
    // Разрешаем доступ только с localhost для тестов
    let allowed_ips = vec![
        "127.0.0.1".parse().unwrap(),
        "::1".parse().unwrap(),
    ];
    Arc::new(AuthManager::new(allowed_ips, Duration::from_secs(3600)))
}

// Глобальный инстанс
pub static CONGESTION_CONTROLLER: Lazy<Arc<CongestionController>> = Lazy::new(|| {
    let analyzer: Arc<dyn TrafficAnalyzer> = Arc::new(TrafficAnalyzerImpl::new());
    let limiter: Arc<dyn AdaptiveLimiter> = Arc::new(AdaptiveLimiterImpl::new());
    let reputation: Arc<dyn ReputationManager> = Arc::new(ReputationManagerImpl::new());
    let monitor: Arc<dyn CongestionMonitor> = Arc::new(ControllerMonitor);
    let auth_manager = create_auth_manager();
    let config_manager = Arc::new(ConfigManager::new());

    // Используем тестовую конфигурацию
    CongestionController::new_for_tests(
        analyzer,
        limiter,
        reputation,
        monitor,
        auth_manager,
        config_manager,
    )
});

pub static CONGESTION_STATE: Lazy<RwLock<ControllerState>> = Lazy::new(|| {
    RwLock::new(ControllerState::default())
});

// Утилиты для удобной работы
pub async fn global_check_packet(
    source_ip: IpAddr,
    packet_data: &[u8],
    packet_priority: PacketPriority,
    connection_metrics: &ConnectionMetrics,
    session_id: Option<Vec<u8>>,
) -> Decision {
    CONGESTION_CONTROLLER.should_accept_packet(
        source_ip,
        packet_data,
        packet_priority,
        connection_metrics,
        session_id,
    ).await
}

pub async fn global_notify_connection_opened(ip: IpAddr, session_id: Vec<u8>) {
    CONGESTION_CONTROLLER.on_connection_opened(ip, session_id).await;
}

pub async fn global_notify_connection_closed(ip: IpAddr, session_id: Vec<u8>) {
    CONGESTION_CONTROLLER.on_connection_closed(ip, session_id).await;
}

pub async fn global_set_enabled(enabled: bool) {
    if enabled {
        CONGESTION_CONTROLLER.enable().await;
    } else {
        CONGESTION_CONTROLLER.disable().await;
    }
    CONGESTION_STATE.write().await.is_enabled = enabled;
}

pub async fn global_set_learning_mode(enabled: bool) {
    CONGESTION_CONTROLLER.set_learning_mode(enabled).await;
    CONGESTION_STATE.write().await.learning_mode = enabled;
}

// Административные функции с аутентификацией
pub async fn global_admin_enable(session_id: String, client_ip: IpAddr) -> Result<(), String> {
    CONGESTION_CONTROLLER.admin_enable(session_id, client_ip).await
        .map_err(|e| e.to_string())
}

pub async fn global_admin_disable(session_id: String, client_ip: IpAddr) -> Result<(), String> {
    CONGESTION_CONTROLLER.admin_disable(session_id, client_ip).await
        .map_err(|e| e.to_string())
}

pub async fn global_admin_set_learning_mode(session_id: String, client_ip: IpAddr, enabled: bool) -> Result<(), String> {
    CONGESTION_CONTROLLER.admin_set_learning_mode(session_id, client_ip, enabled).await
        .map_err(|e| e.to_string())
}

pub async fn global_admin_ban_ip(session_id: String, client_ip: IpAddr, target_ip: IpAddr, duration: Option<Duration>) -> Result<(), String> {
    CONGESTION_CONTROLLER.admin_ban_ip(session_id, client_ip, target_ip, duration).await
        .map_err(|e| e.to_string())
}

pub async fn global_admin_whitelist_ip(session_id: String, client_ip: IpAddr, target_ip: IpAddr, enabled: bool) -> Result<(), String> {
    CONGESTION_CONTROLLER.admin_whitelist_ip(session_id, client_ip, target_ip, enabled).await
        .map_err(|e| e.to_string())
}

pub async fn global_get_state() -> ControllerState {
    CONGESTION_STATE.read().await.clone()
}