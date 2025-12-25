use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::{warn, info};

use crate::core::protocol::server::security::security_audit::SecurityAudit;
use crate::core::protocol::server::security::security_metrics::SecurityMetrics;
use crate::core::protocol::server::security::rate_limiter::classifier::PacketPriority;

use super::auth::{AuthManager, AuthError};
use super::config::{ConfigManager, SecurityConfig};
use super::health_check::HealthState;
use super::validation::InputValidator;

use super::traits::{
    TrafficAnalyzer, AdaptiveLimiter, ReputationManager, CongestionMonitor,
    CongestionStats
};
use super::types::{
    Decision, PacketInfo, ConnectionMetrics, LoadLevel,
    ReputationScore, AnomalyScore
};

/// Главный контроллер congestion control системы
pub struct CongestionController {
    analyzer: Arc<dyn TrafficAnalyzer>,
    limiter: Arc<dyn AdaptiveLimiter>,
    reputation: Arc<dyn ReputationManager>,
    monitor: Arc<dyn CongestionMonitor>,
    auth_manager: Arc<AuthManager>,
    config_manager: Arc<ConfigManager>,

    // Конфигурация теперь в RwLock для изменяемости
    config: RwLock<ControllerConfig>,

    // Внутреннее состояние
    is_enabled: RwLock<bool>,
}

#[derive(Debug, Clone)]
pub struct ControllerConfig {
    pub enabled: bool,
    pub learning_mode: bool,
    pub max_decision_time: Duration,
    pub auto_update_interval: Duration,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            learning_mode: false,
            max_decision_time: Duration::from_millis(10),
            auto_update_interval: Duration::from_secs(5),
        }
    }
}

impl CongestionController {
    pub fn new(
        analyzer: Arc<dyn TrafficAnalyzer>,
        limiter: Arc<dyn AdaptiveLimiter>,
        reputation: Arc<dyn ReputationManager>,
        monitor: Arc<dyn CongestionMonitor>,
        auth_manager: Arc<AuthManager>,
        config: ControllerConfig,
        config_manager: Arc<ConfigManager>,
    ) -> Arc<Self> {
        let controller = Arc::new(Self {
            analyzer,
            limiter,
            reputation,
            monitor,
            auth_manager,
            config: RwLock::new(config),
            is_enabled: RwLock::new(true),
            config_manager,
        });

        // Запускаем фоновые задачи
        controller.clone().start_background_tasks();
        controller
    }

    /// ОСНОВНОЙ МЕТОД - должен ли быть принят пакет
    pub async fn should_accept_packet(
        &self,
        source_ip: IpAddr,
        packet_data: &[u8],
        packet_priority: PacketPriority,
        connection_metrics: &ConnectionMetrics,
        session_id: Option<Vec<u8>>,
    ) -> Decision {
        let _timer = SecurityMetrics::processing_time().start_timer();

        // Проверяем включена ли система
        if !*self.is_enabled.read().await {
            return Decision::Accept;
        }

        let config = self.config_manager.get_config().await;
        let allow_private_ips = config.test_mode.enabled && config.test_mode.allow_private_ips;

        let packet_info = PacketInfo {
            source_ip,
            size: packet_data.len(),
            priority: packet_priority,
            timestamp: Instant::now(),
            session_id: session_id.clone(),
        };

        // Валидация входных данных с учетом тестового режима
        if let Err(e) = InputValidator::validate_ip(source_ip, allow_private_ips) {
            warn!("Invalid IP address {}: {}", source_ip, e);

            // В тестовом режиме не баним, только логируем
            if config.test_mode.enabled && config.test_mode.relaxed_validation {
                info!("Test mode: Allowing invalid IP {}", source_ip);
            } else {
                return Decision::TempBan(Duration::from_secs(60));
            }
        }

        if let Err(e) = InputValidator::validate_packet_size(packet_data.len()) {
            warn!("Invalid packet size {}: {}", packet_data.len(), e);
            return Decision::RateLimit(Duration::from_secs(5));
        }

        if let Some(session_id) = &session_id {
            if let Err(e) = InputValidator::validate_session_id(session_id) {
                warn!("Invalid session ID: {}", e);
                return Decision::RateLimit(Duration::from_secs(1));
            }
        }

        // 1. Быстрая проверка репутации IP
        let reputation = self.reputation.get_score(source_ip).await;
        if reputation.is_blacklisted {
            self.monitor.record_decision(source_ip, &Decision::PermanentBan, "blacklisted").await;
            SecurityAudit::log_congestion_decision(source_ip, "permanent_ban", "blacklisted").await;
            return Decision::PermanentBan;
        }

        if reputation.is_whitelisted {
            self.monitor.record_decision(source_ip, &Decision::Accept, "whitelisted").await;
            return Decision::Accept;
        }

        // 2. Анализ трафика на аномалии
        let anomaly_score = self.analyzer.analyze_packet(&packet_info, connection_metrics).await;

        // 3. Принятие финального решения
        let decision = self.limiter.make_decision(
            packet_info.clone(),
            reputation,
            anomaly_score.clone(),
        ).await;

        // 4. Логирование и мониторинг
        let reason = self.generate_decision_reason(&anomaly_score, &decision);
        self.monitor.record_decision(source_ip, &decision, &reason).await;

        // Получаем конфиг для проверки режима обучения
        let config = self.config.read().await;

        // В режиме обучения не применяем баны, только логируем
        if config.learning_mode {
            match decision {
                Decision::Accept => Decision::Accept,
                _ => {
                    SecurityAudit::log_congestion_decision(
                        source_ip,
                        "learning_mode_override",
                        &format!("Would apply: {:?}", decision)
                    ).await;
                    Decision::Accept
                }
            }
        } else {
            // Логируем серьезные решения
            if !matches!(decision, Decision::Accept) {
                SecurityAudit::log_congestion_decision(source_ip, "applied", &reason).await;
            }
            decision
        }
    }

    /// Уведомление о новом соединении
    pub async fn on_connection_opened(&self, ip: IpAddr, session_id: Vec<u8>) {
        self.analyzer.on_connection_opened(ip, session_id.clone()).await;
        self.reputation.on_connection_opened(ip).await;

        SecurityAudit::log_connection_event(ip, "opened", &hex::encode(&session_id)).await;
    }

    /// Уведомление о закрытии соединения
    pub async fn on_connection_closed(&self, ip: IpAddr, session_id: Vec<u8>) {
        self.analyzer.on_connection_closed(ip, session_id.clone()).await;

        SecurityAudit::log_connection_event(ip, "closed", &hex::encode(&session_id)).await;
    }

    /// Административные методы с аутентификацией
    pub async fn enable(&self) {
        *self.is_enabled.write().await = true;
    }

    pub async fn disable(&self) {
        *self.is_enabled.write().await = false;
    }

    pub async fn set_learning_mode(&self, enabled: bool) {
        let mut config = self.config.write().await;
        config.learning_mode = enabled;
    }

    // Методы с аутентификацией (для внешнего API)
    pub async fn admin_enable(&self, session_id: String, client_ip: IpAddr) -> Result<(), AuthError> {
        self.auth_manager.check_permission(&session_id, client_ip, "change_config").await?;

        *self.is_enabled.write().await = true;
        SecurityAudit::log_congestion_decision(client_ip, "controller_enabled", "by_admin").await;
        Ok(())
    }

    pub async fn admin_disable(&self, session_id: String, client_ip: IpAddr) -> Result<(), AuthError> {
        self.auth_manager.check_permission(&session_id, client_ip, "change_config").await?;

        *self.is_enabled.write().await = false;
        SecurityAudit::log_congestion_decision(client_ip, "controller_disabled", "by_admin").await;
        Ok(())
    }

    pub async fn admin_set_learning_mode(&self, session_id: String, client_ip: IpAddr, enabled: bool) -> Result<(), AuthError> {
        self.auth_manager.check_permission(&session_id, client_ip, "change_config").await?;

        let mut config = self.config.write().await;
        config.learning_mode = enabled;

        SecurityAudit::log_congestion_decision(
            client_ip,
            "learning_mode_changed",
            &format!("enabled={}", enabled)
        ).await;
        Ok(())
    }

    pub async fn admin_ban_ip(&self, session_id: String, client_ip: IpAddr, target_ip: IpAddr, duration: Option<Duration>) -> Result<(), AuthError> {
        self.auth_manager.check_permission(&session_id, client_ip, "ban_ips").await?;

        // Валидация IP
        Self::validate_admin_ip(target_ip)?;

        self.reputation.ban_ip(target_ip, duration).await;

        SecurityAudit::log_congestion_decision(
            client_ip,
            "ip_banned",
            &format!("target={}, duration={:?}", target_ip, duration)
        ).await;
        Ok(())
    }

    pub async fn admin_whitelist_ip(&self, session_id: String, client_ip: IpAddr, target_ip: IpAddr, enabled: bool) -> Result<(), AuthError> {
        self.auth_manager.check_permission(&session_id, client_ip, "whitelist_ips").await?;

        // Валидация IP
        Self::validate_admin_ip(target_ip)?;

        self.reputation.whitelist_ip(target_ip, enabled).await;

        SecurityAudit::log_congestion_decision(
            client_ip,
            "ip_whitelist_changed",
            &format!("target={}, enabled={}", target_ip, enabled)
        ).await;
        Ok(())
    }

    // Публичные методы без аутентификации (для внутреннего использования)
    pub async fn ban_ip(&self, ip: IpAddr, duration: Option<Duration>) {
        self.reputation.ban_ip(ip, duration).await;
    }

    pub async fn whitelist_ip(&self, ip: IpAddr, enabled: bool) {
        self.reputation.whitelist_ip(ip, enabled).await;
    }

    pub async fn get_system_load(&self) -> LoadLevel {
        self.analyzer.get_system_load().await
    }

    pub async fn get_ip_reputation(&self, ip: IpAddr) -> ReputationScore {
        self.reputation.get_score(ip).await
    }

    pub fn new_for_tests(
        analyzer: Arc<dyn TrafficAnalyzer>,
        limiter: Arc<dyn AdaptiveLimiter>,
        reputation: Arc<dyn ReputationManager>,
        monitor: Arc<dyn CongestionMonitor>,
        auth_manager: Arc<AuthManager>,
        config_manager: Arc<ConfigManager>,
    ) -> Arc<Self> {
        let test_config = ControllerConfig {
            enabled: true,
            learning_mode: true, // В режиме обучения не баним
            max_decision_time: Duration::from_millis(5),
            auto_update_interval: Duration::from_secs(10), // Реже обновляем для тестов
        };

        Arc::new(Self {
            analyzer,
            limiter,
            reputation,
            monitor,
            auth_manager,
            config: RwLock::new(test_config),
            is_enabled: RwLock::new(true),
            config_manager,
        })
    }

    pub async fn health_check(&self) -> HealthState {
        let start_time = Instant::now();

        // Проверяем доступность компонентов
        let checks = vec![
            self.check_analyzer_health().await,
            self.check_limiter_health().await,
            self.check_reputation_health().await,
            self.check_database_health().await,
        ];

        let mut unhealthy_count = 0;
        let mut degraded_count = 0;

        for check in checks {
            match check {
                HealthState::Unhealthy => unhealthy_count += 1,
                HealthState::Degraded => degraded_count += 1,
                HealthState::Healthy => {}
            }
        }

        let overall_health = if unhealthy_count > 0 {
            HealthState::Unhealthy
        } else if degraded_count > 0 {
            HealthState::Degraded
        } else {
            HealthState::Healthy
        };

        let response_time = start_time.elapsed();

        // Логируем медленные health checks
        if response_time > Duration::from_millis(100) {
            warn!("Health check took too long: {:?}", response_time);
        }

        overall_health
    }

    // Приватные методы
    async fn check_analyzer_health(&self) -> HealthState {
        match self.analyzer.get_system_load().await {
            LoadLevel::Normal | LoadLevel::High => HealthState::Healthy,
            LoadLevel::Critical => HealthState::Degraded,
            LoadLevel::UnderAttack => HealthState::Unhealthy,
        }
    }

    async fn check_limiter_health(&self) -> HealthState {
        // Простая проверка - пытаемся получить текущие лимиты
        match tokio::time::timeout(
            Duration::from_millis(100),
            self.limiter.get_current_limits()
        ).await {
            Ok(_) => HealthState::Healthy,
            Err(_) => HealthState::Degraded,
        }
    }

    async fn check_reputation_health(&self) -> HealthState {
        // Проверяем базу данных репутации
        HealthState::Healthy // Упрощенная проверка
    }

    async fn check_database_health(&self) -> HealthState {
        HealthState::Healthy // Упрощенная проверка
    }

    async fn _get_config(&self) -> SecurityConfig {
        self.config_manager.get_config().await
    }

    fn start_background_tasks(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                // Получаем интервал из конфига
                let update_interval = {
                    let config = self.config.read().await;
                    config.auto_update_interval
                };

                // Ждем указанный интервал
                tokio::time::sleep(update_interval).await;

                let load_level = self.analyzer.get_system_load().await;
                self.limiter.update_limits(load_level.clone()).await;
                self.monitor.update_load_metrics(load_level).await;
            }
        });
    }

    fn generate_decision_reason(&self, anomaly_score: &AnomalyScore, decision: &Decision) -> String {
        // Не раскрываем детали внутренней логики в логах
        match decision {
            Decision::Accept => "normal_traffic".to_string(),
            Decision::RateLimit(_) => {
                // Обобщенная причина без деталей скора
                if anomaly_score.score > 0.7 {
                    "high_anomaly_rate_limit".to_string()
                } else if anomaly_score.score > 0.4 {
                    "medium_anomaly_rate_limit".to_string()
                } else {
                    "low_anomaly_rate_limit".to_string()
                }
            }
            Decision::TempBan(_) => {
                if anomaly_score.score > 0.8 {
                    "critical_anomaly_temp_ban".to_string()
                } else {
                    "severe_anomaly_temp_ban".to_string()
                }
            }
            Decision::PermanentBan => "permanent_ban_reputation".to_string(),
        }
    }

    fn validate_admin_ip(ip: IpAddr) -> Result<(), AuthError> {
        // Запрещаем операции с важными IP-адресами
        if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
            return Err(AuthError::InsufficientPermissions);
        }

        Ok(())
    }
}

// Убираем реализацию Clone - она больше не нужна
// Вместо этого контроллер всегда используется через Arc

// Реализация CongestionMonitor для контроллера
pub struct ControllerMonitor;

#[async_trait]
impl CongestionMonitor for ControllerMonitor {
    async fn record_decision(&self, _ip: IpAddr, decision: &Decision, _reason: &str) {
        match decision {
            Decision::Accept => {
                SecurityMetrics::congestion_accepted().inc();
            }
            Decision::RateLimit(_) => {
                SecurityMetrics::congestion_rate_limited().inc();
            }
            Decision::TempBan(_) | Decision::PermanentBan => {
                SecurityMetrics::congestion_banned().inc();
            }
        }
    }

    async fn update_load_metrics(&self, load_level: LoadLevel) {
        let level_value = match load_level {
            LoadLevel::Normal => 0.0,
            LoadLevel::High => 1.0,
            LoadLevel::Critical => 2.0,
            LoadLevel::UnderAttack => 3.0,
        };
        SecurityMetrics::system_load().set(level_value);
    }

    async fn get_stats(&self, _period: Duration) -> CongestionStats {
        CongestionStats {
            total_decisions: 0,
            accepted_packets: 0,
            rate_limited: 0,
            banned: 0,
            avg_processing_time: Duration::default(),
        }
    }
}