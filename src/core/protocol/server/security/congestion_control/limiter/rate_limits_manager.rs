use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Instant, Duration};
use tokio::sync::RwLock;

use crate::core::protocol::server::security::congestion_control::types::LoadLevel;
use super::aimd_algorithm::AIMDAlgorithm;

#[derive(Debug, Clone)]
pub struct IPRateLimits {
    pub packets_per_second: u64,
    pub bytes_per_second: u64,
    pub connections_per_minute: u64,
    pub burst_multiplier: f64,
    pub last_updated: Instant,
}

pub struct RateLimitsManager {
    base_limits: IPRateLimits,
    ip_limits: RwLock<HashMap<IpAddr, IPRateLimits>>,
    aimd: AIMDAlgorithm,
    load_multipliers: RwLock<HashMap<LoadLevel, f64>>,
}

impl RateLimitsManager {
    pub fn new(base_limits: IPRateLimits, aimd: AIMDAlgorithm) -> Self {
        let mut load_multipliers = HashMap::new();
        load_multipliers.insert(LoadLevel::Normal, 1.0);
        load_multipliers.insert(LoadLevel::High, 0.7);    // Ужесточаем на 30%
        load_multipliers.insert(LoadLevel::Critical, 0.4); // Ужесточаем на 60%
        load_multipliers.insert(LoadLevel::UnderAttack, 0.1); // Ужесточаем на 90%

        Self {
            base_limits,
            ip_limits: RwLock::new(HashMap::new()),
            aimd,
            load_multipliers: RwLock::new(load_multipliers),
        }
    }

    pub async fn get_limits_for_ip(&self, ip: IpAddr, load_level: LoadLevel) -> IPRateLimits {
        let base_multiplier = self.get_load_multiplier(load_level).await;
        let aimd_limit = self.aimd.get_limit(ip).await;

        // ИСПРАВЛЕНИЕ: Проверяем кэшированные лимиты для IP
        let cached_limits = {
            let ip_limits = self.ip_limits.read().await;
            ip_limits.get(&ip).cloned()
        };

        if let Some(cached) = cached_limits {
            // Если лимиты устарели (старше 5 секунд), пересчитываем
            if cached.last_updated.elapsed() < Duration::from_secs(5) {
                return cached;
            }
        }

        let mut limits = self.base_limits.clone();

        // Применяем множитель нагрузки
        limits.packets_per_second = (limits.packets_per_second as f64 * base_multiplier) as u64;
        limits.bytes_per_second = (limits.bytes_per_second as f64 * base_multiplier) as u64;

        // Применяем AIMD лимит
        limits.packets_per_second = limits.packets_per_second.min(aimd_limit);
        limits.last_updated = Instant::now();

        // ИСПРАВЛЕНИЕ: Сохраняем в кэш
        {
            let mut ip_limits = self.ip_limits.write().await;
            ip_limits.insert(ip, limits.clone());
        }

        limits
    }

    pub async fn cleanup_old_limits(&self) {
        let mut ip_limits = self.ip_limits.write().await;
        let now = Instant::now();

        ip_limits.retain(|_, limits| now.duration_since(limits.last_updated) < Duration::from_secs(30));
    }

    // Добавляем метод для принудительного сброса лимитов IP
    pub async fn reset_ip_limits(&self, ip: IpAddr) {
        let mut ip_limits = self.ip_limits.write().await;
        ip_limits.remove(&ip);
        self.aimd.reset_window(ip).await;
    }

    pub async fn update_limits_for_load(&self, load_level: LoadLevel) {
        // Обновляем базовые лимиты based на нагрузке
        let multiplier = self.get_load_multiplier(load_level.clone()).await;
        println!("Updated limits for load level {:?} with multiplier {}", load_level, multiplier);
    }

    pub async fn record_success(&self, ip: IpAddr) {
        self.aimd.record_success(ip).await;
    }

    pub async fn record_failure(&self, ip: IpAddr) {
        self.aimd.record_failure(ip).await;
    }

    async fn get_load_multiplier(&self, load_level: LoadLevel) -> f64 {
        let multipliers = self.load_multipliers.read().await;
        *multipliers.get(&load_level).unwrap_or(&1.0) // Исправлено: добавляем ссылку и &
    }

    pub async fn set_load_multiplier(&self, load_level: LoadLevel, multiplier: f64) {
        let mut multipliers = self.load_multipliers.write().await;
        multipliers.insert(load_level, multiplier);
    }
}

impl Default for IPRateLimits {
    fn default() -> Self {
        Self {
            packets_per_second: 1000,
            bytes_per_second: 1024 * 1024, // 1 MB/s
            connections_per_minute: 10,
            burst_multiplier: 1.5,
            last_updated: Instant::now(),
        }
    }
}