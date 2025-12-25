use std::time::Duration;

use crate::core::protocol::server::security::congestion_control::types::{
    Decision, PacketInfo, ReputationScore, AnomalyScore
};

#[derive(Debug, Clone)]
pub struct DecisionRules {
    pub anomaly_threshold_soft: f64,  // Порог для rate limiting
    pub anomaly_threshold_hard: f64,  // Порог для бана
    pub min_ban_duration: Duration,
    pub max_ban_duration: Duration,
}

impl Default for DecisionRules {
    fn default() -> Self {
        Self {
            anomaly_threshold_soft: 0.3,  // 30% - начинаем ограничивать
            anomaly_threshold_hard: 0.7,  // 70% - бан
            min_ban_duration: Duration::from_secs(30),
            max_ban_duration: Duration::from_secs(300), // 5 минут
        }
    }
}

pub struct DecisionEngine {
    rules: DecisionRules,
}

impl DecisionEngine {
    pub fn new(rules: DecisionRules) -> Self {
        Self { rules }
    }

    pub fn new_for_tests() -> Self {
        let rules = DecisionRules {
            anomaly_threshold_soft: 0.8,  // Выше порог для тестов
            anomaly_threshold_hard: 0.95, // Очень высокий порог для бана
            min_ban_duration: Duration::from_secs(1), // Короткие баны
            max_ban_duration: Duration::from_secs(5),
        };
        Self { rules }
    }

    pub fn make_decision(
        &self,
        _packet: &PacketInfo,
        reputation: &ReputationScore,
        anomaly_score: &AnomalyScore,
    ) -> Decision {
        // 1. Проверка репутации (высший приоритет)
        if reputation.is_blacklisted {
            return Decision::PermanentBan;
        }

        if reputation.is_whitelisted {
            return Decision::Accept;
        }

        // 2. Учет репутационного скора
        let reputation_factor = reputation.score; // 0.0 (хороший) - 1.0 (плохой)
        let combined_score = anomaly_score.score.max(reputation_factor);

        // 3. Принятие решения на основе комбинированного скора
        if combined_score >= self.rules.anomaly_threshold_hard {
            let ban_duration = self.calculate_ban_duration(combined_score);
            Decision::TempBan(ban_duration)
        } else if combined_score >= self.rules.anomaly_threshold_soft {
            let rate_limit_duration = self.calculate_rate_limit_duration(combined_score);
            Decision::RateLimit(rate_limit_duration)
        } else {
            Decision::Accept
        }
    }

    fn calculate_ban_duration(&self, score: f64) -> Duration {
        let ratio = (score - self.rules.anomaly_threshold_hard) / (1.0 - self.rules.anomaly_threshold_hard);
        let duration_secs = self.rules.min_ban_duration.as_secs() as f64 +
            ratio * (self.rules.max_ban_duration.as_secs() - self.rules.min_ban_duration.as_secs()) as f64;

        Duration::from_secs(duration_secs as u64)
    }

    fn calculate_rate_limit_duration(&self, score: f64) -> Duration {
        let ratio = (score - self.rules.anomaly_threshold_soft) /
            (self.rules.anomaly_threshold_hard - self.rules.anomaly_threshold_soft);

        // От 100ms до 5 секунд
        Duration::from_millis(100 + (ratio * 4900.0) as u64)
    }

    pub fn should_apply_grace_period(&self, _ip: &str, first_offense: bool) -> bool {
        // Для первых нарушений даем шанс
        first_offense
    }
}