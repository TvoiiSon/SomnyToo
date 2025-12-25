use std::net::IpAddr;
use std::time::Duration;
use async_trait::async_trait;
use super::types::{
    PacketInfo, ConnectionMetrics, LoadLevel,
    ReputationScore, AnomalyScore, OffenseSeverity, Decision
};

// Трейт для анализатора трафика - обнаруживает аномалии и DDoS атаки
#[async_trait]
pub trait TrafficAnalyzer: Send + Sync {
    /// Анализирует пакет на аномальность (0.0 = норма, 1.0 = атака)
    async fn analyze_packet(
        &self,
        packet: &PacketInfo,
        metrics: &ConnectionMetrics,
    ) -> AnomalyScore;

    /// Возвращает текущий уровень нагрузки системы
    async fn get_system_load(&self) -> LoadLevel;

    /// Уведомление о новом соединении
    async fn on_connection_opened(&self, ip: IpAddr, session_id: Vec<u8>);

    /// Уведомление о закрытии соединения
    async fn on_connection_closed(&self, ip: IpAddr, session_id: Vec<u8>);

    /// Возвращает статистику по IP за период
    async fn get_ip_stats(&self, ip: IpAddr, period: Duration) -> ConnectionMetrics;
}

// Трейт для адаптивного лимитера - принимает решения на основе анализа
#[async_trait]
pub trait AdaptiveLimiter: Send + Sync {
    /// Основной метод принятия решения по пакету
    async fn make_decision(
        &self,
        packet: PacketInfo,
        reputation: ReputationScore,
        anomaly_score: AnomalyScore,
    ) -> Decision;

    /// Обновляет лимиты based на текущей нагрузке системы
    async fn update_limits(&self, load_level: LoadLevel);

    /// Возвращает текущие лимиты для отладки/мониторинга
    async fn get_current_limits(&self) -> RateLimits;

    /// Сбрасывает лимиты для конкретного IP (при ошибках)
    async fn reset_limits_for_ip(&self, ip: IpAddr);
}

// Трейт для системы репутации IP - отслеживает "плохие" IP
#[async_trait]
pub trait ReputationManager: Send + Sync {
    /// Возвращает репутационный score для IP
    async fn get_score(&self, ip: IpAddr) -> ReputationScore;

    /// Сообщает о нарушении от IP
    async fn report_offense(&self, ip: IpAddr, severity: OffenseSeverity);

    /// Бан IP на указанное время (None = перманентно)
    async fn ban_ip(&self, ip: IpAddr, duration: Option<Duration>);

    /// Добавляет/удаляет IP из whitelist
    async fn whitelist_ip(&self, ip: IpAddr, enabled: bool);

    /// Уведомление о новом соединении (для статистики)
    async fn on_connection_opened(&self, ip: IpAddr);

    /// Удаляет IP из blacklist/whitelist
    async fn remove_ip(&self, ip: IpAddr);
}

// Дополнительные типы для лимитов
#[derive(Debug, Clone)]
pub struct RateLimits {
    pub packets_per_second: u64,
    pub bytes_per_second: u64,
    pub connections_per_minute: u64,
    pub burst_multiplier: f64,
}

// Трейт для мониторинга и метрик congestion control
#[async_trait]
pub trait CongestionMonitor: Send + Sync {
    /// Регистрирует принятое решение
    async fn record_decision(&self, ip: IpAddr, decision: &Decision, reason: &str);

    /// Обновляет метрики нагрузки
    async fn update_load_metrics(&self, load_level: LoadLevel);

    /// Возвращает статистику за период
    async fn get_stats(&self, period: Duration) -> CongestionStats;
}

#[derive(Debug, Clone)]
pub struct CongestionStats {
    pub total_decisions: u64,
    pub accepted_packets: u64,
    pub rate_limited: u64,
    pub banned: u64,
    pub avg_processing_time: Duration,
}