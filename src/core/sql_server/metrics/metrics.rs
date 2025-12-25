use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use dashmap::DashMap;
use tokio::sync::Mutex;
use serde::Serialize;

#[derive(Debug, Serialize, Clone)]
pub struct ServerMetrics {
    pub total_queries: u64,
    pub successful_queries: u64,
    pub failed_queries: u64,
    pub average_response_time: f64,
    pub queries_per_second: f64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub active_connections: usize,
    pub prepared_statements_count: usize,
    pub batch_queries_processed: u64,
}

#[derive(Debug, Clone)]
pub struct QueryMetrics {
    pub query_type: String,
    pub execution_time: Duration,
    pub rows_affected: u64,
    pub timestamp: Instant,
    pub client_ip: String,
    pub used_cache: bool,
    pub used_replica: bool,
}

pub struct MetricsCollector {
    metrics: Arc<MetricsData>,
    query_history: Arc<DashMap<String, Vec<QueryMetrics>>>,
    start_time: Instant,
}

struct MetricsData {
    total_queries: AtomicU64,
    successful_queries: AtomicU64,
    failed_queries: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    batch_queries_processed: AtomicU64,
    active_connections: AtomicUsize,
    prepared_statements_count: AtomicUsize,
    response_times: Mutex<Vec<Duration>>,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            metrics: Arc::new(MetricsData {
                total_queries: AtomicU64::new(0),
                successful_queries: AtomicU64::new(0),
                failed_queries: AtomicU64::new(0),
                cache_hits: AtomicU64::new(0),
                cache_misses: AtomicU64::new(0),
                batch_queries_processed: AtomicU64::new(0),
                active_connections: AtomicUsize::new(0),
                prepared_statements_count: AtomicUsize::new(0),
                response_times: Mutex::new(Vec::with_capacity(1000)),
            }),
            query_history: Arc::new(DashMap::new()),
            start_time: Instant::now(),
        }
    }

    pub fn record_query_start(&self) -> QueryTimer {
        self.metrics.total_queries.fetch_add(1, Ordering::Relaxed);
        self.metrics.active_connections.fetch_add(1, Ordering::Relaxed);

        QueryTimer {
            start_time: Instant::now(),
            query_type: String::new(),
            client_ip: String::new(),
        }
    }

    pub fn record_query_success(&self, query_metrics: QueryMetrics) {
        self.metrics.successful_queries.fetch_add(1, Ordering::Relaxed);
        self.metrics.active_connections.fetch_sub(1, Ordering::Relaxed);

        let response_times = self.metrics.response_times.try_lock();
        if let Ok(mut times) = response_times {
            times.push(query_metrics.execution_time);
            if times.len() > 1000 {
                times.drain(0..100);
            }
        }

        // Сохраняем историю запросов
        let query_key = format!("{}:{}", query_metrics.query_type, &query_metrics.client_ip[..8]);
        self.query_history.entry(query_key)
            .or_insert_with(Vec::new)
            .push(query_metrics);
    }

    pub fn record_query_failure(&self) {
        self.metrics.failed_queries.fetch_add(1, Ordering::Relaxed);
        self.metrics.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn record_cache_hit(&self) {
        self.metrics.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cache_miss(&self) {
        self.metrics.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_batch_processed(&self, count: u64) {
        self.metrics.batch_queries_processed.fetch_add(count, Ordering::Relaxed);
    }

    pub fn update_prepared_statements_count(&self, count: usize) {
        self.metrics.prepared_statements_count.store(count, Ordering::Relaxed);
    }

    pub fn get_metrics(&self) -> ServerMetrics {
        let total_queries = self.metrics.total_queries.load(Ordering::Relaxed);
        let uptime = self.start_time.elapsed().as_secs_f64();

        let response_times = self.metrics.response_times.try_lock()
            .map(|times| times.clone())
            .unwrap_or_default();

        let average_response_time = if !response_times.is_empty() {
            response_times.iter().map(|d| d.as_secs_f64()).sum::<f64>() / response_times.len() as f64
        } else {
            0.0
        };

        ServerMetrics {
            total_queries,
            successful_queries: self.metrics.successful_queries.load(Ordering::Relaxed),
            failed_queries: self.metrics.failed_queries.load(Ordering::Relaxed),
            average_response_time,
            queries_per_second: total_queries as f64 / uptime.max(1.0),
            cache_hits: self.metrics.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.metrics.cache_misses.load(Ordering::Relaxed),
            active_connections: self.metrics.active_connections.load(Ordering::Relaxed),
            prepared_statements_count: self.metrics.prepared_statements_count.load(Ordering::Relaxed),
            batch_queries_processed: self.metrics.batch_queries_processed.load(Ordering::Relaxed),
        }
    }

    pub fn get_query_stats(&self, query_type: &str) -> QueryTypeStats {
        let mut stats = QueryTypeStats::new(query_type);

        for entry in self.query_history.iter() {
            if entry.key().starts_with(query_type) {
                for metric in entry.value() {
                    stats.total_count += 1;
                    stats.total_execution_time += metric.execution_time;
                    if metric.used_cache {
                        stats.cache_hits += 1;
                    }
                    if metric.used_replica {
                        stats.replica_usage += 1;
                    }
                }
            }
        }

        if stats.total_count > 0 {
            stats.average_time = stats.total_execution_time / stats.total_count as u32;
        }

        stats
    }
}

pub struct QueryTimer {
    start_time: Instant,
    query_type: String,
    client_ip: String,
}

impl QueryTimer {
    pub fn set_query_type(mut self, query_type: &str) -> Self {
        self.query_type = query_type.to_string();
        self
    }

    pub fn set_client_ip(mut self, client_ip: &str) -> Self {
        self.client_ip = client_ip.to_string();
        self
    }

    pub fn finish(self, rows_affected: u64, used_cache: bool, used_replica: bool) -> QueryMetrics {
        let execution_time = self.start_time.elapsed();

        QueryMetrics {
            query_type: self.query_type,
            execution_time,
            rows_affected,
            timestamp: self.start_time,
            client_ip: self.client_ip,
            used_cache,
            used_replica,
        }
    }
}

#[derive(Debug, Clone)]
pub struct QueryTypeStats {
    pub query_type: String,
    pub total_count: u64,
    pub average_time: Duration,
    pub total_execution_time: Duration,
    pub cache_hits: u64,
    pub replica_usage: u64,
}

impl QueryTypeStats {
    pub fn new(query_type: &str) -> Self {
        Self {
            query_type: query_type.to_string(),
            total_count: 0,
            average_time: Duration::default(),
            total_execution_time: Duration::default(),
            cache_hits: 0,
            replica_usage: 0,
        }
    }
}