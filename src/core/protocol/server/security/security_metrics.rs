use prometheus::{Counter, Gauge, Histogram, Registry, HistogramOpts, Result as PrometheusResult};

#[derive(Clone)]
pub struct SecurityMetrics {
    replay_attacks: Counter,
    rate_limit_hits: Counter,
    failed_handshakes: Counter,
    successful_sessions: Counter,
    active_connections: Gauge,
    processing_time: Histogram,
    nonce_cache_size: Gauge,
    congestion_accepted: Counter,
    congestion_rate_limited: Counter,
    congestion_banned: Counter,
    system_load: Gauge,
}

impl SecurityMetrics {
    pub fn new() -> PrometheusResult<Self> {
        Ok(Self {
            replay_attacks: Counter::new("replay_attacks_total", "Total replay attacks detected")?,
            rate_limit_hits: Counter::new("rate_limit_hits_total", "Total rate limit hits")?,
            failed_handshakes: Counter::new("failed_handshakes_total", "Total failed handshakes")?,
            successful_sessions: Counter::new("successful_sessions_total", "Total successful sessions")?,
            active_connections: Gauge::new("active_connections", "Current active connections")?,
            processing_time: Histogram::with_opts(HistogramOpts::new("processing_time_seconds", "Request processing time"))?,
            nonce_cache_size: Gauge::new("nonce_cache_size", "Current nonce cache size")?,
            congestion_accepted: Counter::new("congestion_accepted_total", "Total packets accepted by congestion control")?,
            congestion_rate_limited: Counter::new("congestion_rate_limited_total", "Total packets rate limited by congestion control")?,
            congestion_banned: Counter::new("congestion_banned_total", "Total packets banned by congestion control")?,
            system_load: Gauge::new("system_load", "Current system load level")?,
        })
    }

    pub fn register(registry: &Registry) -> anyhow::Result<()> {
        let metrics = Self::new()?;
        registry.register(Box::new(metrics.replay_attacks.clone()))?;
        registry.register(Box::new(metrics.rate_limit_hits.clone()))?;
        registry.register(Box::new(metrics.failed_handshakes.clone()))?;
        registry.register(Box::new(metrics.successful_sessions.clone()))?;
        registry.register(Box::new(metrics.active_connections.clone()))?;
        registry.register(Box::new(metrics.processing_time.clone()))?;
        registry.register(Box::new(metrics.nonce_cache_size.clone()))?;
        registry.register(Box::new(metrics.congestion_accepted.clone()))?;
        registry.register(Box::new(metrics.congestion_rate_limited.clone()))?;
        registry.register(Box::new(metrics.congestion_banned.clone()))?;
        registry.register(Box::new(metrics.system_load.clone()))?;
        Ok(())
    }

    // Геттеры для метрик с обработкой ошибок
    pub fn replay_attacks() -> Counter {
        Self::new().unwrap_or_else(|e| panic!("Failed to create replay_attacks counter: {}", e)).replay_attacks
    }

    pub fn rate_limit_hits() -> Counter {
        Self::new().unwrap_or_else(|e| panic!("Failed to create rate_limit_hits counter: {}", e)).rate_limit_hits
    }

    pub fn failed_handshakes() -> Counter {
        Self::new().unwrap_or_else(|e| panic!("Failed to create failed_handshakes counter: {}", e)).failed_handshakes
    }

    pub fn successful_sessions() -> Counter {
        Self::new().unwrap_or_else(|e| panic!("Failed to create successful_sessions counter: {}", e)).successful_sessions
    }

    pub fn active_connections() -> Gauge {
        Self::new().unwrap_or_else(|e| panic!("Failed to create active_connections gauge: {}", e)).active_connections
    }

    pub fn processing_time() -> Histogram {
        Self::new().unwrap_or_else(|e| panic!("Failed to create processing_time histogram: {}", e)).processing_time
    }

    pub fn nonce_cache_size() -> Gauge {
        Self::new().unwrap_or_else(|e| panic!("Failed to create nonce_cache_size gauge: {}", e)).nonce_cache_size
    }

    pub fn congestion_accepted() -> Counter {
        Self::new().unwrap_or_else(|e| panic!("Failed to create congestion_accepted counter: {}", e)).congestion_accepted
    }

    pub fn congestion_rate_limited() -> Counter {
        Self::new().unwrap_or_else(|e| panic!("Failed to create congestion_rate_limited counter: {}", e)).congestion_rate_limited
    }

    pub fn congestion_banned() -> Counter {
        Self::new().unwrap_or_else(|e| panic!("Failed to create congestion_banned counter: {}", e)).congestion_banned
    }

    pub fn system_load() -> Gauge {
        Self::new().unwrap_or_else(|e| panic!("Failed to create system_load gauge: {}", e)).system_load
    }
}