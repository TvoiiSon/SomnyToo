use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use std::sync::Arc;
use tracing::info;

#[derive(Debug, Clone)]
pub struct IPRecord {
    pub ip: IpAddr,
    pub first_seen: Instant,
    pub last_seen: Instant,
    pub total_offenses: u32,
    pub is_whitelisted: bool,
    pub is_blacklisted: bool,
    pub blacklist_reason: Option<String>,
    pub blacklist_until: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct CleanupInfo {
    pub total_records: usize,
    pub cleanup_interval: Duration,
    pub max_capacity: usize,
}

#[derive(Debug, Clone)]
pub struct CleanupResult {
    pub records_removed: usize,
    pub before_stats: DatabaseStats,
    pub after_stats: DatabaseStats,
}

pub struct IPDatabase {
    records: RwLock<HashMap<IpAddr, IPRecord>>,
    cleanup_interval: Duration,
    max_records: usize, // Ограничение максимального количества записей
}

impl IPDatabase {
    pub fn new(cleanup_interval: Duration) -> Self {
        Self {
            records: RwLock::new(HashMap::new()),
            cleanup_interval,
            max_records: 100_000, // Максимум 100k записей
        }
    }

    pub async fn start_periodic_cleanup(self: Arc<Self>) {
        let cleanup_interval = self.cleanup_interval;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(cleanup_interval);

            loop {
                interval.tick().await;
                let removed_count = self.cleanup_expired_records().await;

                if removed_count > 0 {
                    info!("Cleaned up {} expired IP records", removed_count);
                }
            }
        });
    }

    pub async fn get_cleanup_info(&self) -> CleanupInfo {
        let stats = self.get_stats().await;

        CleanupInfo {
            total_records: stats.total_records,
            cleanup_interval: self.cleanup_interval,
            max_capacity: stats.max_capacity,
        }
    }

    // Добавляем метод для настройки интервала очистки
    pub fn set_cleanup_interval(&mut self, interval: Duration) {
        self.cleanup_interval = interval;
    }

    // Добавляем метод для принудительной очистки с возвратом статистики
    pub async fn force_cleanup(&self) -> CleanupResult {
        let before_stats = self.get_stats().await;
        let removed_count = self.cleanup_expired_records().await;
        let after_stats = self.get_stats().await;

        CleanupResult {
            records_removed: removed_count,
            before_stats,
            after_stats,
        }
    }

    pub fn with_capacity(cleanup_interval: Duration, max_records: usize) -> Self {
        Self {
            records: RwLock::new(HashMap::with_capacity(max_records.min(100_000))),
            cleanup_interval,
            max_records,
        }
    }

    pub async fn get_record(&self, ip: IpAddr) -> Option<IPRecord> {
        let records = self.records.read().await;
        records.get(&ip).cloned() // Быстрое чтение без блокировки
    }

    pub async fn get_or_create_record(&self, ip: IpAddr) -> Result<IPRecord, String> {
        // Сначала быстрая проверка без блокировки записи
        {
            let records = self.records.read().await;
            if let Some(record) = records.get(&ip) {
                return Ok(record.clone());
            }
        }

        // Только если записи нет, блокируем для записи
        let mut records = self.records.write().await;

        // Проверяем лимит записей
        if records.len() >= self.max_records {
            return Err("IP database capacity exceeded".to_string());
        }

        let record = IPRecord {
            ip,
            first_seen: Instant::now(),
            last_seen: Instant::now(),
            total_offenses: 0,
            is_whitelisted: false,
            is_blacklisted: false,
            blacklist_reason: None,
            blacklist_until: None,
        };

        records.insert(ip, record.clone());
        Ok(record)
    }

    pub async fn update_record(&self, ip: IpAddr, update_fn: impl FnOnce(&mut IPRecord)) -> Result<(), String> {
        let mut records = self.records.write().await;

        if let Some(record) = records.get_mut(&ip) {
            update_fn(record);
            record.last_seen = Instant::now();
            Ok(())
        } else {
            Err("IP record not found".to_string())
        }
    }

    pub async fn whitelist_ip(&self, ip: IpAddr, enabled: bool) -> Result<(), String> {
        self.update_record(ip, |record| {
            record.is_whitelisted = enabled;
            if enabled {
                record.is_blacklisted = false;
                record.blacklist_reason = None;
                record.blacklist_until = None;
            }
        }).await
    }

    pub async fn blacklist_ip(&self, ip: IpAddr, duration: Option<Duration>, reason: String) -> Result<(), String> {
        self.update_record(ip, |record| {
            record.is_blacklisted = true;
            record.blacklist_reason = Some(reason);
            record.blacklist_until = duration.map(|d| Instant::now() + d);
        }).await
    }

    pub async fn remove_from_blacklist(&self, ip: IpAddr) -> Result<(), String> {
        self.update_record(ip, |record| {
            record.is_blacklisted = false;
            record.blacklist_reason = None;
            record.blacklist_until = None;
        }).await
    }

    pub async fn increment_offenses(&self, ip: IpAddr) -> Result<u32, String> {
        let mut offenses = 0;

        self.update_record(ip, |record| {
            record.total_offenses = record.total_offenses.saturating_add(1); // Защита от переполнения
            offenses = record.total_offenses;
        }).await?;

        Ok(offenses)
    }

    pub async fn cleanup_expired_records(&self) -> usize {
        let mut records = self.records.write().await;
        let now = Instant::now();
        let initial_count = records.len();

        records.retain(|_, record| {
            // Удаляем записи старше 30 дней, если не в blacklist/whitelist
            if record.is_blacklisted || record.is_whitelisted {
                return true;
            }

            // Проверяем expired blacklist
            if let Some(until) = record.blacklist_until {
                if now > until {
                    return false; // Удаляем expired blacklist записи
                }
            }

            // Удаляем если запись старше 30 дней и нет нарушений
            now.duration_since(record.last_seen) < Duration::from_secs(30 * 24 * 60 * 60)
        });

        initial_count - records.len()
    }

    pub async fn get_blacklisted_ips(&self) -> Vec<IPRecord> {
        let records = self.records.read().await;
        records.values()
            .filter(|record| record.is_blacklisted)
            .cloned()
            .collect()
    }

    pub async fn get_whitelisted_ips(&self) -> Vec<IPRecord> {
        let records = self.records.read().await;
        records.values()
            .filter(|record| record.is_whitelisted)
            .cloned()
            .collect()
    }

    pub async fn get_stats(&self) -> DatabaseStats {
        let records = self.records.read().await;
        DatabaseStats {
            total_records: records.len(),
            blacklisted: records.values().filter(|r| r.is_blacklisted).count(),
            whitelisted: records.values().filter(|r| r.is_whitelisted).count(),
            max_capacity: self.max_records,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DatabaseStats {
    pub total_records: usize,
    pub blacklisted: usize,
    pub whitelisted: usize,
    pub max_capacity: usize,
}