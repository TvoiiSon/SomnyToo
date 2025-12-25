use std::time::{Duration, Instant};
use dashmap::DashMap;
use tokio::sync::RwLock;
use std::sync::Arc;

use super::error::SecurityError;

pub struct RateLimiter {
    requests: Arc<DashMap<String, Vec<Instant>>>,
    max_requests_per_minute: usize,
    cleanup_interval: Duration,
    last_cleanup: Arc<RwLock<Instant>>,
    cleanup_count: std::sync::atomic::AtomicU64,
}

impl RateLimiter {
    pub fn new(max_requests: usize, cleanup_interval: Duration) -> Self {
        Self {
            requests: Arc::new(DashMap::new()),
            max_requests_per_minute: max_requests,
            cleanup_interval,
            last_cleanup: Arc::new(RwLock::new(Instant::now())),
            cleanup_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub async fn check_limit(&self, client_ip: &str) -> Result<(), SecurityError> {
        let now = Instant::now();

        // ✅ ИСПРАВЛЕНО: Используем cleanup_interval для определения частоты очистки
        self.cleanup_expired(now).await;

        let mut entry = self.requests.entry(client_ip.to_string()).or_insert_with(Vec::new);

        // Удаляем записи старше 60 секунд
        entry.retain(|&time| now.duration_since(time) < Duration::from_secs(60));

        if entry.len() >= self.max_requests_per_minute {
            return Err(SecurityError::RateLimitExceeded);
        }

        entry.push(now);
        Ok(())
    }

    async fn cleanup_expired(&self, now: Instant) {
        // ✅ ИСПРАВЛЕНО: Используем cleanup_interval для определения когда чистить
        {
            let last_cleanup = self.last_cleanup.read().await;
            if now.duration_since(*last_cleanup) < self.cleanup_interval {
                return; // Еще рано чистить
            }
        }

        // Обновляем время последней очистки
        {
            let mut last_cleanup = self.last_cleanup.write().await;
            *last_cleanup = now;
        }

        let before_count = self.requests.len();

        // ✅ ВЫПОЛНЯЕМ ОЧИСТКУ С ИСПОЛЬЗОВАНИЕМ ИНТЕРВАЛА
        self.requests.retain(|_, times| {
            times.retain(|&time| now.duration_since(time) < Duration::from_secs(120));
            !times.is_empty()
        });

        let after_count = self.requests.len();
        let cleaned_count = before_count.saturating_sub(after_count);

        if cleaned_count > 0 {
            self.cleanup_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            println!("[RateLimiter] Cleaned {} expired entries (interval: {:?})", cleaned_count, self.cleanup_interval);
        }
    }

    // ✅ ДОБАВЛЕНО: Методы для доступа к cleanup_interval
    pub fn get_cleanup_interval(&self) -> Duration {
        self.cleanup_interval
    }

    pub fn set_cleanup_interval(&mut self, interval: Duration) {
        self.cleanup_interval = interval;
        println!("[RateLimiter] Cleanup interval updated to: {:?}", interval);
    }

    pub fn get_cleanup_count(&self) -> u64 {
        self.cleanup_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    // ✅ ДОБАВЛЕНО: Принудительная очистка
    pub async fn force_cleanup(&self) -> usize {
        let now = Instant::now();
        let before_count = self.requests.len();

        self.requests.retain(|_, times| {
            times.retain(|&time| now.duration_since(time) < Duration::from_secs(120));
            !times.is_empty()
        });

        let after_count = self.requests.len();
        let cleaned = before_count - after_count;

        if cleaned > 0 {
            self.cleanup_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        cleaned
    }

    // ✅ ДОБАВЛЕНО: Получение статистики
    pub async fn get_stats(&self) -> RateLimiterStats {
        let now = Instant::now();
        let mut total_clients = 0;
        let mut total_requests = 0;
        let mut blocked_clients = 0;

        for entry in self.requests.iter() {
            total_clients += 1;
            let requests_count = entry.value().iter()
                .filter(|&&time| now.duration_since(time) < Duration::from_secs(60))
                .count();

            total_requests += requests_count;
            if requests_count >= self.max_requests_per_minute {
                blocked_clients += 1;
            }
        }

        RateLimiterStats {
            total_clients,
            total_requests,
            blocked_clients,
            max_requests_per_minute: self.max_requests_per_minute,
            cleanup_count: self.get_cleanup_count(),
            cleanup_interval: self.cleanup_interval,
        }
    }

    // ✅ ДОБАВЛЕНО: Метод для сброса лимитов для конкретного IP
    pub async fn reset_limit(&self, client_ip: &str) {
        self.requests.remove(client_ip);
        println!("[RateLimiter] Reset limit for IP: {}", client_ip);
    }

    // ✅ ДОБАВЛЕНО: Метод для настройки лимитов на лету
    pub fn set_max_requests(&mut self, max_requests: usize) {
        self.max_requests_per_minute = max_requests;
        println!("[RateLimiter] Max requests updated to: {}", max_requests);
    }
}

// ✅ ДОБАВЛЕНО: Статистика лимитера
pub struct RateLimiterStats {
    pub total_clients: usize,
    pub total_requests: usize,
    pub blocked_clients: usize,
    pub max_requests_per_minute: usize,
    pub cleanup_count: u64,
    pub cleanup_interval: Duration,
}