use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use lru::LruCache;
use dashmap::DashMap;
use std::num::NonZeroUsize;

pub struct MultiLevelCache {
    l1_cache: Arc<Mutex<LruCache<String, (String, Instant)>>>,
    l2_cache: Arc<DashMap<String, (String, Instant)>>,
    external_cache_enabled: bool,
    ttl: Duration,
}

impl MultiLevelCache {
    pub fn new(l1_capacity: usize, ttl: Duration) -> Self {
        let capacity = NonZeroUsize::new(l1_capacity.max(1)).unwrap();

        Self {
            l1_cache: Arc::new(Mutex::new(LruCache::new(capacity))),
            l2_cache: Arc::new(DashMap::new()),
            external_cache_enabled: false,
            ttl,
        }
    }

    pub async fn get(&self, key: &str) -> Option<String> {
        let now = Instant::now();

        // Уровень 1: LRU кэш
        let mut l1_cache = self.l1_cache.lock().await;
        if let Some((value, timestamp)) = l1_cache.get(key) {
            if now.duration_since(*timestamp) < self.ttl {
                return Some(value.clone());
            } else {
                // Удаляем просроченный элемент из L1
                l1_cache.pop(key);
            }
        }
        drop(l1_cache); // Явно освобождаем блокировку

        // Уровень 2: DashMap кэш
        if let Some(entry) = self.l2_cache.get(key) {
            let (value, timestamp) = entry.clone();
            if now.duration_since(timestamp) < self.ttl {
                // Обновляем L1 кэш
                let mut l1_cache = self.l1_cache.lock().await;
                l1_cache.put(key.to_string(), (value.clone(), timestamp));
                return Some(value);
            } else {
                // Удаляем просроченный ключ
                self.l2_cache.remove(key);
            }
        }

        None
    }

    pub async fn set(&self, key: String, value: String) {
        let now = Instant::now();

        // Сохраняем во всех уровнях
        let mut l1_cache = self.l1_cache.lock().await;
        l1_cache.put(key.clone(), (value.clone(), now));
        drop(l1_cache); // Освобождаем блокировку

        self.l2_cache.insert(key, (value, now));
    }

    pub async fn set_batch(&self, items: Vec<(String, String)>) {
        let now = Instant::now();
        let mut l1_cache = self.l1_cache.lock().await;

        for (key, value) in items {
            l1_cache.put(key.clone(), (value.clone(), now));
            self.l2_cache.insert(key, (value, now));
        }
    }

    pub async fn clear_expired(&self) {
        let now = Instant::now();

        // Очищаем L1
        {
            let mut l1_cache = self.l1_cache.lock().await;
            l1_cache.iter_mut().for_each(|(_, (_, timestamp))| {
                if now.duration_since(*timestamp) >= self.ttl {
                    // Помечаем для удаления
                }
            });
            // LRU cache автоматически вытесняет старые элементы
        }

        // Очищаем L2
        self.l2_cache.retain(|_, (_, timestamp)| {
            now.duration_since(*timestamp) < self.ttl
        });
    }

    pub fn enable_external_cache(&mut self) {
        self.external_cache_enabled = true;
    }

    pub async fn get_stats(&self) -> CacheStats {
        let l1_cache = self.l1_cache.lock().await;
        CacheStats {
            l1_size: l1_cache.len(),
            l2_size: self.l2_cache.len(),
            external_enabled: self.external_cache_enabled,
        }
    }
}

pub struct CacheStats {
    pub l1_size: usize,
    pub l2_size: usize,
    pub external_enabled: bool,
}

// Специализированный кэш для запросов
pub struct QueryResultCache {
    cache: MultiLevelCache,
    query_timeout: Duration,
}

impl QueryResultCache {
    pub fn new(capacity: usize, query_timeout: Duration) -> Self {
        Self {
            cache: MultiLevelCache::new(capacity, Duration::from_secs(300)),
            query_timeout,
        }
    }

    pub async fn get_query_result(&self, query: &str, params: &[&str]) -> Option<String> {
        let cache_key = self.generate_cache_key(query, params);
        self.cache.get(&cache_key).await
    }

    pub async fn set_query_result(&self, query: &str, params: &[&str], result: String) {
        let cache_key = self.generate_cache_key(query, params);
        self.cache.set(cache_key, result).await;
    }

    pub fn should_cache_query(&self, execution_time: Duration) -> bool {
        execution_time < self.query_timeout
    }

    fn generate_cache_key(&self, query: &str, params: &[&str]) -> String {
        let mut key = query.to_string();
        for param in params {
            key.push_str(param);
        }
        format!("{:x}", md5::compute(key))
    }

    pub async fn get_stats(&self) -> CacheStats {
        self.cache.get_stats().await
    }
}