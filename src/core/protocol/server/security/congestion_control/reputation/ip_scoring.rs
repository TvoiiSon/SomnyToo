use std::net::IpAddr;
use std::time::{Duration, Instant};

use crate::core::protocol::server::security::congestion_control::types::ReputationScore;

use super::ip_database::IPRecord;
use super::offense_tracker::OffenseTracker;

pub struct ScoreCalculator {
    decay_rate: f64, // Скорость decay скора (в единицах/секунду)
    max_offense_weight: f64,
    time_weight: f64,
}

impl Default for ScoreCalculator {
    fn default() -> Self {
        Self {
            decay_rate: 0.1, // 10% decay в секунду
            max_offense_weight: 0.8,
            time_weight: 0.5,
        }
    }
}

impl ScoreCalculator {
    pub fn calculate_score(
        &self,
        ip_record: &IPRecord,
        _offense_tracker: &OffenseTracker,
    ) -> f64 {
        // Базовый score based на количестве нарушений
        let offense_score = self.calculate_offense_score(ip_record);

        // Временной decay
        let time_factor = self.calculate_time_factor(ip_record);

        // Комбинированный score
        let raw_score = offense_score * (1.0 - time_factor);

        raw_score.min(1.0).max(0.0)
    }

    fn calculate_offense_score(&self, ip_record: &IPRecord) -> f64 {
        let total_offenses = ip_record.total_offenses as f64;
        let offense_weight = (total_offenses.ln_1p() / 10.0).min(self.max_offense_weight);
        offense_weight
    }

    fn calculate_time_factor(&self, ip_record: &IPRecord) -> f64 {
        let time_since_last_offense = Instant::now().duration_since(ip_record.last_seen);
        let seconds_passed = time_since_last_offense.as_secs_f64();
        let decay_factor = 1.0 - (-self.decay_rate * seconds_passed).exp();
        decay_factor * self.time_weight
    }
}

pub struct IPScoring {
    calculator: ScoreCalculator,
}

impl IPScoring {
    pub fn new(calculator: ScoreCalculator) -> Self {
        Self { calculator }
    }

    pub async fn calculate_reputation_score(
        &self,
        ip: IpAddr,
        ip_database: &super::ip_database::IPDatabase,
        offense_tracker: &OffenseTracker,
    ) -> ReputationScore {
        // Получаем запись с обработкой ошибок
        let ip_record_result = ip_database.get_or_create_record(ip).await;

        let ip_record = match ip_record_result {
            Ok(record) => record,
            Err(e) => {
                // В случае ошибки возвращаем дефолтный score
                eprintln!("Failed to get IP record for {}: {}", ip, e);
                return ReputationScore {
                    score: 0.5,
                    offenses: 0,
                    last_offense: None,
                    is_whitelisted: false,
                    is_blacklisted: false,
                };
            }
        };

        let score = if ip_record.is_whitelisted {
            0.0
        } else if ip_record.is_blacklisted {
            1.0
        } else {
            // Теперь передаем ссылку на IPRecord
            self.calculator.calculate_score(&ip_record, offense_tracker)
        };

        let _recent_offenses = offense_tracker.get_offense_count(ip, None).await;

        ReputationScore {
            score,
            offenses: ip_record.total_offenses,
            last_offense: if ip_record.total_offenses > 0 {
                Some(ip_record.last_seen)
            } else {
                None
            },
            is_whitelisted: ip_record.is_whitelisted,
            is_blacklisted: ip_record.is_blacklisted,
        }
    }

    pub fn should_auto_blacklist(&self, score: f64, recent_offenses: u32) -> bool {
        // Автоматический blacklist при высоком score и множественных нарушениях
        score > 0.8 && recent_offenses >= 5
    }

    pub fn calculate_ban_duration(&self, score: f64, offense_count: u32) -> Duration {
        let base_duration = 60; // 1 минута базово
        let multiplier = (score * 10.0).max(1.0) * (offense_count as f64).sqrt();

        Duration::from_secs((base_duration as f64 * multiplier) as u64)
            .min(Duration::from_secs(24 * 60 * 60)) // Максимум 24 часа
    }
}