use std::collections::{VecDeque, HashMap};
use std::net::IpAddr;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use crate::core::protocol::server::security::congestion_control::types::OffenseSeverity;

#[derive(Debug, Clone)]
pub struct OffenseRecord {
    pub ip: IpAddr,
    pub severity: OffenseSeverity,
    pub timestamp: Instant,
    pub description: String,
}

pub struct OffenseTracker {
    offenses: RwLock<HashMap<IpAddr, VecDeque<OffenseRecord>>>,
    max_offenses_per_ip: usize,
    max_total_offenses: usize, // Общее максимальное количество нарушений
    offense_window: Duration,
    cleanup_interval: Duration,
    last_cleanup: RwLock<Instant>,
}

impl OffenseTracker {
    pub fn new(max_offenses_per_ip: usize, offense_window: Duration) -> Self {
        Self {
            offenses: RwLock::new(HashMap::new()),
            max_offenses_per_ip,
            max_total_offenses: 100_000, // Общий лимит нарушений
            offense_window,
            cleanup_interval: Duration::from_secs(300), // Cleanup каждые 5 минут
            last_cleanup: RwLock::new(Instant::now()),
        }
    }

    pub fn with_limits(max_offenses_per_ip: usize, max_total_offenses: usize, offense_window: Duration) -> Self {
        Self {
            offenses: RwLock::new(HashMap::new()),
            max_offenses_per_ip,
            max_total_offenses,
            offense_window,
            cleanup_interval: Duration::from_secs(300),
            last_cleanup: RwLock::new(Instant::now()),
        }
    }

    pub async fn record_offense(&self, ip: IpAddr, severity: OffenseSeverity, description: String) -> Result<(), String> {
        // Периодическая очистка старых записей
        self.auto_cleanup().await;

        let mut offenses = self.offenses.write().await;

        // Проверяем общий лимит нарушений
        let total_offenses: usize = offenses.values().map(|v| v.len()).sum();
        if total_offenses >= self.max_total_offenses {
            return Err("Maximum total offenses limit reached".to_string());
        }

        let offense_list = offenses.entry(ip).or_insert_with(|| VecDeque::new());

        let offense = OffenseRecord {
            ip,
            severity,
            timestamp: Instant::now(),
            description,
        };

        offense_list.push_back(offense);

        // Удаляем старые нарушения
        self.cleanup_old_offenses(offense_list);

        // Ограничиваем количество записей на IP
        if offense_list.len() > self.max_offenses_per_ip {
            offense_list.pop_front();
        }

        Ok(())
    }

    pub async fn get_recent_offenses(&self, ip: IpAddr) -> Vec<OffenseRecord> {
        let offenses = self.offenses.read().await;
        if let Some(offense_list) = offenses.get(&ip) {
            offense_list.iter().cloned().collect()
        } else {
            Vec::new()
        }
    }

    pub async fn get_offense_count(&self, ip: IpAddr, severity: Option<OffenseSeverity>) -> u32 {
        let offenses = self.get_recent_offenses(ip).await;

        offenses.iter()
            .filter(|offense| {
                if let Some(sev) = &severity {
                    offense.severity == *sev
                } else {
                    true
                }
            })
            .count() as u32
    }

    pub async fn calculate_offense_score(&self, ip: IpAddr) -> f64 {
        let offenses = self.get_recent_offenses(ip).await;
        let now = Instant::now();

        let mut score = 0.0;

        for offense in offenses {
            let time_factor = self.calculate_time_factor(offense.timestamp, now);
            let severity_factor = match offense.severity {
                OffenseSeverity::Minor => 0.3,
                OffenseSeverity::Moderate => 0.5,
                OffenseSeverity::Major => 0.7,
                OffenseSeverity::Critical => 1.0,
            };

            score += severity_factor * time_factor;
        }

        score.min(1.0)
    }

    fn calculate_time_factor(&self, offense_time: Instant, now: Instant) -> f64 {
        let time_passed = now.duration_since(offense_time);
        if time_passed > self.offense_window {
            return 0.0;
        }

        1.0 - (time_passed.as_secs_f64() / self.offense_window.as_secs_f64())
    }

    fn cleanup_old_offenses(&self, offense_list: &mut VecDeque<OffenseRecord>) {
        let now = Instant::now();
        offense_list.retain(|offense| now.duration_since(offense.timestamp) <= self.offense_window);
    }

    async fn auto_cleanup(&self) {
        let now = Instant::now();
        let last_cleanup = *self.last_cleanup.read().await;

        if now.duration_since(last_cleanup) > self.cleanup_interval {
            if let Ok(mut last_cleanup_write) = self.last_cleanup.try_write() {
                *last_cleanup_write = now;
                drop(last_cleanup_write); // Освобождаем блокировку перед cleanup

                self.cleanup_all_old_offenses().await;
            }
        }
    }

    pub async fn cleanup_all_old_offenses(&self) -> usize {
        let mut offenses = self.offenses.write().await;
        let now = Instant::now();
        let mut total_removed = 0;

        for offense_list in offenses.values_mut() {
            let before_len = offense_list.len();
            offense_list.retain(|offense| now.duration_since(offense.timestamp) <= self.offense_window);
            total_removed += before_len - offense_list.len();
        }

        // Удаляем IP без нарушений
        offenses.retain(|_, list| !list.is_empty());

        total_removed
    }

    pub async fn get_stats(&self) -> OffenseTrackerStats {
        let offenses = self.offenses.read().await;
        let total_offenses: usize = offenses.values().map(|v| v.len()).sum();
        let unique_ips = offenses.len();

        OffenseTrackerStats {
            total_offenses,
            unique_ips,
            max_offenses_per_ip: self.max_offenses_per_ip,
            max_total_offenses: self.max_total_offenses,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OffenseTrackerStats {
    pub total_offenses: usize,
    pub unique_ips: usize,
    pub max_offenses_per_ip: usize,
    pub max_total_offenses: usize,
}