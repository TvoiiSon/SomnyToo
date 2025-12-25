use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct MetricsCache {
    cache: Arc<RwLock<HashMap<String, (Instant, serde_json::Value)>>>,
    ttl: Duration,
}

impl MetricsCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            ttl,
        }
    }

    pub async fn get_or_compute<F, Fut>(&self, key: &str, computer: F) -> serde_json::Value
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = serde_json::Value>,
    {
        let now = Instant::now();
        let mut cache = self.cache.write().await;

        if let Some((timestamp, value)) = cache.get(key) {
            if now.duration_since(*timestamp) < self.ttl {
                return value.clone();
            }
        }

        // Вычисляем новое значение
        let value = computer().await;
        cache.insert(key.to_string(), (now, value.clone()));

        value
    }

    pub async fn invalidate(&self, key: &str) {
        let mut cache = self.cache.write().await;
        cache.remove(key);
    }

    pub async fn clear(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
    }
}