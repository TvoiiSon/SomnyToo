use std::net::IpAddr;
use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::core::protocol::server::security::congestion_control::traits::AdaptiveLimiter;
use crate::core::protocol::server::security::congestion_control::types::{
    Decision, PacketInfo, ReputationScore, AnomalyScore, LoadLevel
};

use super::aimd_algorithm::{AIMDAlgorithm, AIMDConfig};
use super::rate_limits_manager::{RateLimitsManager, IPRateLimits};
use super::decision_engine::{DecisionEngine, DecisionRules};

pub struct AdaptiveLimiterImpl {
    limits_manager: RateLimitsManager,
    decision_engine: DecisionEngine,
    current_limits: RwLock<IPRateLimits>,
    load_level: RwLock<LoadLevel>,
}

impl AdaptiveLimiterImpl {
    pub fn new() -> Self {
        let aimd_config = AIMDConfig::default();
        let aimd_algorithm = AIMDAlgorithm::new(aimd_config);
        let base_limits = IPRateLimits::default();
        let limits_manager = RateLimitsManager::new(base_limits.clone(), aimd_algorithm);

        let decision_rules = DecisionRules::default();
        let decision_engine = DecisionEngine::new(decision_rules);

        Self {
            limits_manager,
            decision_engine,
            current_limits: RwLock::new(base_limits),
            load_level: RwLock::new(LoadLevel::Normal),
        }
    }

    pub fn with_rules(rules: DecisionRules) -> Self {
        let aimd_config = AIMDConfig::default();
        let aimd_algorithm = AIMDAlgorithm::new(aimd_config);
        let base_limits = IPRateLimits::default();
        let limits_manager = RateLimitsManager::new(base_limits.clone(), aimd_algorithm);

        let decision_engine = DecisionEngine::new(rules);

        Self {
            limits_manager,
            decision_engine,
            current_limits: RwLock::new(base_limits),
            load_level: RwLock::new(LoadLevel::Normal),
        }
    }
}

#[async_trait]
impl AdaptiveLimiter for AdaptiveLimiterImpl {
    async fn make_decision(
        &self,
        packet: PacketInfo,
        reputation: ReputationScore,
        anomaly_score: AnomalyScore,
    ) -> Decision {
        let decision = self.decision_engine.make_decision(
            &packet,
            &reputation,
            &anomaly_score,
        );

        // Записываем результат для AIMD алгоритма
        match &decision {
            Decision::Accept => {
                self.limits_manager.record_success(packet.source_ip).await;
            }
            Decision::RateLimit(_) | Decision::TempBan(_) | Decision::PermanentBan => {
                self.limits_manager.record_failure(packet.source_ip).await;
            }
        }

        decision
    }

    async fn update_limits(&self, load_level: LoadLevel) {
        // Обновляем лимиты based на нагрузке
        self.limits_manager.update_limits_for_load(load_level.clone()).await;

        // Сохраняем текущий уровень нагрузки
        *self.load_level.write().await = load_level;
    }

    async fn get_current_limits(&self) -> super::super::traits::RateLimits {
        let limits = self.current_limits.read().await.clone();
        super::super::traits::RateLimits {
            packets_per_second: limits.packets_per_second,
            bytes_per_second: limits.bytes_per_second,
            connections_per_minute: limits.connections_per_minute,
            burst_multiplier: limits.burst_multiplier,
        }
    }

    async fn reset_limits_for_ip(&self, ip: IpAddr) {
        // Сбрасываем статистику для IP (при ошибках или админском вмешательстве)
        // В текущей реализации AIMD сам очищает старые записи
        println!("Reset limits for IP: {}", ip);
    }
}