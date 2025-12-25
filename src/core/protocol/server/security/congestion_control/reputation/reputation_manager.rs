use std::net::IpAddr;
use std::time::Duration;
use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::{error, info};

use crate::core::protocol::server::security::congestion_control::traits::ReputationManager;
use crate::core::protocol::server::security::congestion_control::types::{
    ReputationScore, OffenseSeverity
};

use super::ip_database::IPDatabase;
use super::offense_tracker::OffenseTracker;
use super::ip_scoring::{IPScoring, ScoreCalculator};
use super::reputation_cache::ReputationCache;


pub struct ReputationManagerImpl {
    ip_database: IPDatabase,
    offense_tracker: OffenseTracker,
    scoring: IPScoring,
    auto_blacklist_enabled: RwLock<bool>,
    cache: ReputationCache,
}


impl ReputationManagerImpl {
    pub fn new() -> Self {
        let ip_database = IPDatabase::new(Duration::from_secs(3600)); // Cleanup каждый час
        let offense_tracker = OffenseTracker::new(100, Duration::from_secs(3600)); // 100 нарушений на IP, окно 1 час
        let calculator = ScoreCalculator::default();
        let scoring = IPScoring::new(calculator);
        let cache = ReputationCache::new(10_000, Duration::from_secs(60)); // Кэш на 10k IP, TTL 60 сек

        Self {
            ip_database,
            offense_tracker,
            scoring,
            auto_blacklist_enabled: RwLock::new(true),
            cache
        }
    }

    pub async fn set_auto_blacklist_enabled(&self, enabled: bool) {
        *self.auto_blacklist_enabled.write().await = enabled;

        if enabled {
            info!("Auto-blacklist feature enabled");
        } else {
            info!("Auto-blacklist feature disabled");
        }
    }

    pub async fn is_auto_blacklist_enabled(&self) -> bool {
        *self.auto_blacklist_enabled.read().await
    }

    // ИСПРАВЛЕНИЕ: Интегрируем метод check_auto_blacklist в основной поток
    async fn evaluate_auto_blacklist(&self, ip: IpAddr) -> Result<(), String> {
        if !self.is_auto_blacklist_enabled().await {
            return Ok(());
        }

        let score = self.get_score(ip).await.score;
        let recent_offenses = self.offense_tracker.get_offense_count(ip, None).await;

        if self.scoring.should_auto_blacklist(score, recent_offenses) {
            let ban_duration = self.scoring.calculate_ban_duration(score, recent_offenses);

            self.ip_database.blacklist_ip(
                ip,
                Some(ban_duration),
                format!("Auto-blacklist: score={:.2}, offenses={}", score, recent_offenses)
            ).await?;

            info!("Auto-blacklisted IP {} for {:?} (score: {:.2}, offenses: {})",
                  ip, ban_duration, score, recent_offenses);
        }

        Ok(())
    }

    // Переименовываем и делаем публичным для внешнего использования
    pub async fn check_and_auto_blacklist(&self, ip: IpAddr) -> Result<bool, String> {
        let was_blacklisted = {
            let record = self.ip_database.get_record(ip).await;
            record.map(|r| r.is_blacklisted).unwrap_or(false)
        };

        self.evaluate_auto_blacklist(ip).await?;

        let is_now_blacklisted = {
            let record = self.ip_database.get_record(ip).await;
            record.map(|r| r.is_blacklisted).unwrap_or(false)
        };

        Ok(!was_blacklisted && is_now_blacklisted)
    }

    pub fn with_cleanup_intervals(db_cleanup: Duration, offense_window: Duration) -> Self {
        let ip_database = IPDatabase::new(db_cleanup);
        let offense_tracker = OffenseTracker::new(100, offense_window);
        let calculator = ScoreCalculator::default();
        let scoring = IPScoring::new(calculator);
        let cache = ReputationCache::new(10_000, db_cleanup);

        Self {
            ip_database,
            offense_tracker,
            scoring,
            auto_blacklist_enabled: RwLock::new(true),
            cache,
        }
    }
}

#[async_trait]
impl ReputationManager for ReputationManagerImpl {
    async fn get_score(&self, ip: IpAddr) -> ReputationScore {
        // Пытаемся получить из кэша
        if let Some(cached_score) = self.cache.get(ip).await {
            return cached_score;
        }

        // Создаем или получаем запись IP
        let ip_record = match self.ip_database.get_or_create_record(ip).await {
            Ok(record) => record,
            Err(e) => {
                error!("Failed to get/create record for {}: {}", ip, e);
                // Возвращаем нейтральный score при ошибке
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
            0.0 // Whitelisted IP имеют идеальную репутацию
        } else if ip_record.is_blacklisted {
            1.0 // Blacklisted IP имеют худшую репутацию
        } else {
            self.scoring.calculate_reputation_score(ip, &self.ip_database, &self.offense_tracker).await.score
        };

        let reputation_score = ReputationScore {
            score,
            offenses: ip_record.total_offenses,
            last_offense: if ip_record.total_offenses > 0 {
                Some(ip_record.last_seen)
            } else {
                None
            },
            is_whitelisted: ip_record.is_whitelisted,
            is_blacklisted: ip_record.is_blacklisted,
        };

        // Сохраняем в кэш
        self.cache.put(ip, reputation_score.clone()).await;
        reputation_score
    }

    async fn report_offense(&self, ip: IpAddr, severity: OffenseSeverity) {
        self.cache.invalidate(ip).await;

        let description = match severity {
            OffenseSeverity::Minor => "Minor offense".to_string(),
            OffenseSeverity::Moderate => "Moderate offense".to_string(),
            OffenseSeverity::Major => "Major offense".to_string(),
            OffenseSeverity::Critical => "Critical offense".to_string(),
        };

        // Записываем нарушение
        if let Err(e) = self.offense_tracker.record_offense(ip, severity.clone(), description.clone()).await {
            eprintln!("Failed to record offense: {}", e);
            return;
        }

        // Обновляем счетчик нарушений
        match self.ip_database.increment_offenses(ip).await {
            Ok(total_offenses) => {
                if matches!(severity, OffenseSeverity::Major | OffenseSeverity::Critical) {
                    println!("Offense reported: {} - {} (total: {})", ip, description, total_offenses);

                    // ИСПРАВЛЕНИЕ: Проверяем auto-blacklist после серьезных нарушений
                    if let Err(e) = self.check_and_auto_blacklist(ip).await {
                        eprintln!("Auto-blacklist check failed for {}: {}", ip, e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to increment offenses for {}: {}", ip, e);
            }
        }
    }

    async fn ban_ip(&self, ip: IpAddr, duration: Option<Duration>) {
        let reason = if let Some(dur) = duration {
            format!("Manual ban for {} seconds", dur.as_secs())
        } else {
            "Manual permanent ban".to_string()
        };

        if let Err(e) = self.ip_database.blacklist_ip(ip, duration, reason).await {
            eprintln!("Failed to ban IP {}: {}", ip, e);
        }
    }

    async fn whitelist_ip(&self, ip: IpAddr, enabled: bool) {
        // Сначала пытаемся создать запись если не существует
        match self.ip_database.get_or_create_record(ip).await {
            Ok(_) => {
                // Теперь применяем whitelist
                if let Err(e) = self.ip_database.whitelist_ip(ip, enabled).await {
                    if enabled {
                        error!("Failed to whitelist IP {}: {}", ip, e);
                    } else {
                        error!("Failed to remove IP {} from whitelist: {}", ip, e);
                    }
                } else {
                    if enabled {
                        info!("IP {} added to whitelist", ip);
                    } else {
                        info!("IP {} removed from whitelist", ip);
                    }
                }
            }
            Err(e) => {
                error!("Failed to create record for IP {}: {}", ip, e);
            }
        }

        // Всегда инвалидируем кэш
        self.cache.invalidate(ip).await;
    }

    async fn on_connection_opened(&self, ip: IpAddr) {
        let _ = self.ip_database.get_or_create_record(ip).await; // Игнорируем ошибку
    }

    async fn remove_ip(&self, ip: IpAddr) {
        if let Err(e) = self.ip_database.remove_from_blacklist(ip).await {
            eprintln!("Failed to remove IP {} from blacklist: {}", ip, e);
        }
    }
}