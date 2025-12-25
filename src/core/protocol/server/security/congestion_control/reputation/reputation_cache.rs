use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use lru::LruCache;

use crate::core::protocol::server::security::congestion_control::types::ReputationScore;

pub struct ReputationCache {
    cache: RwLock<LruCache<IpAddr, (ReputationScore, Instant)>>,
    ttl: Duration,
}

impl ReputationCache {
    pub fn new(max_size: usize, ttl: Duration) -> Self {
        let non_zero_size = NonZeroUsize::new(max_size.max(1)).unwrap();
        Self {
            cache: RwLock::new(LruCache::new(non_zero_size)),
            ttl,
        }
    }

    pub async fn get(&self, ip: IpAddr) -> Option<ReputationScore> {
        let mut cache = self.cache.write().await;

        if let Some((score, timestamp)) = cache.get(&ip) {
            if Instant::now().duration_since(*timestamp) < self.ttl {
                // "Трогаем" запись чтобы обновить ее позицию в LRU
                return Some(score.clone());
            } else {
                // Удаляем просроченную запись
                cache.pop(&ip);
            }
        }
        None
    }

    pub async fn put(&self, ip: IpAddr, score: ReputationScore) {
        let mut cache = self.cache.write().await;
        // Используем push вместо insert для LRU
        cache.put(ip, (score, Instant::now()));
    }

    pub async fn invalidate(&self, ip: IpAddr) {
        let mut cache = self.cache.write().await;
        cache.pop(&ip);
    }

    pub async fn clear(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
    }

    pub async fn stats(&self) -> CacheStats {
        let cache = self.cache.read().await;
        let now = Instant::now();

        // Подсчитываем действительные записи
        let valid_entries = cache.iter()
            .filter(|(_, (_, timestamp))| now.duration_since(*timestamp) < self.ttl)
            .count();

        CacheStats {
            size: valid_entries,
            max_size: cache.cap().get(),
            ttl: self.ttl,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub size: usize,
    pub max_size: usize,
    pub ttl: Duration,
}