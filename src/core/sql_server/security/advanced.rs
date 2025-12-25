use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::{Duration, Instant};

use super::super::config::SecurityConfig;
use super::error::{SecurityError};
use super::rate_limiter::RateLimiter;
use super::sql_injection::SqlInjectionDetector;

#[derive(Clone)]
pub struct AdvancedSecurityLayer {
    rate_limiter: Arc<RateLimiter>,
    sql_injection_detector: Arc<SqlInjectionDetector>,
    query_analyzer: Arc<QueryAnalyzer>,
    ip_blocker: Arc<IpBlocker>,
    query_patterns: Arc<RwLock<HashMap<String, QueryPattern>>>,
}

impl AdvancedSecurityLayer {
    pub fn new(security_config: SecurityConfig) -> Result<Self, SecurityError> {
        let rate_limiter = Arc::new(RateLimiter::new(
            security_config.max_requests_per_minute,
            Duration::from_secs(60)
        ));

        let sql_injection_detector = Arc::new(SqlInjectionDetector::new());
        let query_analyzer = Arc::new(QueryAnalyzer::new(security_config.allowed_tables));
        let ip_blocker = Arc::new(IpBlocker::new(
            security_config.blocked_ips,
            Duration::from_secs(3600)
        ));

        Ok(Self {
            rate_limiter,
            sql_injection_detector,
            query_analyzer,
            ip_blocker,
            query_patterns: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    pub async fn validate_query(&self, query: &str, client_ip: &str) -> Result<(), SecurityError> {
        // Проверяем блокировку IP
        if let Ok(ip) = client_ip.parse::<IpAddr>() {
            if self.ip_blocker.is_blocked(&ip).await {
                return Err(SecurityError::IpBlocked);
            }
        }

        // Проверяем лимит запросов
        self.rate_limiter.check_limit(client_ip).await?;

        // Проверяем SQL инъекции
        self.sql_injection_detector.detect(query)?;

        // Анализируем запрос
        let analysis = self.query_analyzer.analyze(query)?;

        self.analyze_query_pattern(client_ip, &analysis).await?;

        Ok(())
    }

    async fn analyze_query_pattern(&self, client_ip: &str, analysis: &QueryAnalysis) -> Result<(), SecurityError> {
        let mut patterns = self.query_patterns.write().await;
        let pattern_key = format!("{}:{}", client_ip, analysis.operation);

        let pattern = patterns.entry(pattern_key.clone())
            .or_insert_with(|| QueryPattern::new());

        pattern.record_query(analysis);

        // Проверяем на аномальную активность
        if pattern.is_anomalous() {
            // Автоматически блокируем IP при подозрительной активности
            if let Ok(ip) = client_ip.parse::<IpAddr>() {
                self.ip_blocker.block_ip(ip).await;
            }
            return Err(SecurityError::InvalidQueryPattern);
        }

        Ok(())
    }

    pub async fn block_ip(&self, ip: IpAddr) {
        self.ip_blocker.block_ip(ip).await;
    }

    pub async fn get_security_stats(&self) -> SecurityStats {
        let patterns = self.query_patterns.read().await;
        SecurityStats {
            tracked_patterns: patterns.len(),
            blocked_ips_count: self.ip_blocker.get_blocked_count().await,
        }
    }
}

pub struct QueryAnalyzer {
    max_query_size: usize,
    allowed_tables: Vec<String>,
}

impl QueryAnalyzer {
    pub fn new(allowed_tables: Vec<String>) -> Self {
        Self {
            max_query_size: 1024 * 1024, // 1MB
            allowed_tables,
        }
    }

    pub fn analyze(&self, query: &str) -> Result<QueryAnalysis, SecurityError> {
        if query.len() > self.max_query_size {
            return Err(SecurityError::QueryTooLarge);
        }

        let upper_query = query.to_uppercase();
        let operation = if upper_query.starts_with("SELECT") {
            "SELECT".to_string()
        } else if upper_query.starts_with("INSERT") {
            "INSERT".to_string()
        } else if upper_query.starts_with("UPDATE") {
            "UPDATE".to_string()
        } else if upper_query.starts_with("DELETE") {
            "DELETE".to_string()
        } else {
            return Err(SecurityError::InvalidOperation);
        };

        if !self.allowed_tables.is_empty() {
            let mut table_allowed = false;
            for table in &self.allowed_tables {
                if upper_query.contains(&format!(" {} ", table.to_uppercase())) {
                    table_allowed = true;
                    break;
                }
            }
            if !table_allowed {
                return Err(SecurityError::TableNotAllowed);
            }
        }

        Ok(QueryAnalysis {
            operation,
            query_length: query.len(),
            has_joins: upper_query.contains("JOIN"),
            has_subqueries: upper_query.contains("SELECT") && upper_query.matches("SELECT").count() > 1,
            has_dangerous_operations: upper_query.contains("DROP") || upper_query.contains("TRUNCATE"),
        })
    }
}

#[derive(Debug)]
pub struct QueryAnalysis {
    pub operation: String,
    pub query_length: usize,
    pub has_joins: bool,
    pub has_subqueries: bool,
    pub has_dangerous_operations: bool,
}

pub struct QueryPattern {
    pub query_count: u64,
    pub last_query_time: Instant,
    pub average_query_length: f64,
    pub dangerous_operation_count: u64,
}

impl QueryPattern {
    pub fn new() -> Self {
        Self {
            query_count: 0,
            last_query_time: Instant::now(),
            average_query_length: 0.0,
            dangerous_operation_count: 0,
        }
    }

    pub fn record_query(&mut self, analysis: &QueryAnalysis) {
        self.query_count += 1;
        self.last_query_time = Instant::now();

        // Обновляем среднюю длину запроса
        self.average_query_length = (self.average_query_length * (self.query_count - 1) as f64
            + analysis.query_length as f64) / self.query_count as f64;

        if analysis.has_dangerous_operations {
            self.dangerous_operation_count += 1;
        }
    }

    pub fn is_anomalous(&self) -> bool {
        // Обнаружение аномальной активности
        self.query_count > 1000 || // Слишком много запросов
            self.dangerous_operation_count > 10 || // Слишком много опасных операций
            (Instant::now().duration_since(self.last_query_time).as_secs() < 1 && self.query_count > 100) // Взрывная активность
    }
}

pub struct IpBlocker {
    blocked_ips: Arc<RwLock<HashMap<IpAddr, Instant>>>,
    initial_blocked_ips: Vec<IpAddr>,
    block_duration: Duration,
}

impl IpBlocker {
    pub fn new(initial_blocked_ips: Vec<IpAddr>, block_duration: Duration) -> Self {
        let mut blocked_ips = HashMap::new();
        let now = Instant::now();

        for ip in &initial_blocked_ips {
            blocked_ips.insert(*ip, now);
        }

        Self {
            blocked_ips: Arc::new(RwLock::new(blocked_ips)),
            initial_blocked_ips,
            block_duration,
        }
    }

    pub async fn is_blocked(&self, ip: &IpAddr) -> bool {
        let blocked_ips = self.blocked_ips.read().await;
        if let Some(block_time) = blocked_ips.get(ip) {
            if Instant::now().duration_since(*block_time) < self.block_duration {
                return true;
            }
        }
        false
    }

    pub async fn block_ip(&self, ip: IpAddr) {
        let mut blocked_ips = self.blocked_ips.write().await;
        blocked_ips.insert(ip, Instant::now());
    }

    pub async fn unblock_ip(&self, ip: IpAddr) {
        let mut blocked_ips = self.blocked_ips.write().await;
        blocked_ips.remove(&ip);
    }

    pub async fn cleanup_expired_blocks(&self) {
        let mut blocked_ips = self.blocked_ips.write().await;
        let now = Instant::now();
        blocked_ips.retain(|ip, time| {
            // Не удаляем изначально заблокированные IP
            if self.initial_blocked_ips.contains(ip) {
                true
            } else {
                now.duration_since(*time) < self.block_duration
            }
        });
    }

    pub async fn get_blocked_count(&self) -> usize {
        let blocked_ips = self.blocked_ips.read().await;
        blocked_ips.len()
    }
}

pub struct SecurityStats {
    pub tracked_patterns: usize,
    pub blocked_ips_count: usize,
}