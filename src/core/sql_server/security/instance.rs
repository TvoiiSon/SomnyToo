use std::sync::Arc;

use super::error::SecurityError;
use super::rate_limiter::RateLimiter;
use super::sql_injection::SqlInjectionDetector;
use super::sanitizer::QuerySanitizer;

#[derive(Clone)]
pub struct SecurityLayer {
    rate_limiter: Arc<RateLimiter>,
    sql_injection_detector: Arc<SqlInjectionDetector>,
    query_sanitizer: Arc<QuerySanitizer>,
}

impl SecurityLayer {
    pub fn new() -> Self {
        let rate_limiter = Arc::new(RateLimiter::new(1000, std::time::Duration::from_secs(60)));
        let sql_injection_detector = Arc::new(SqlInjectionDetector::new());

        // ✅ ИСПРАВЛЕНО: Добавляем второй аргумент - allowed_tables
        let query_sanitizer = Arc::new(QuerySanitizer::new(1024 * 1024, Vec::new()));

        Self {
            rate_limiter,
            sql_injection_detector,
            query_sanitizer,
        }
    }

    pub async fn check_query(&self, query: &str, client_ip: &str) -> Result<(), SecurityError> {
        self.rate_limiter.check_limit(client_ip).await?;
        self.sql_injection_detector.detect(query)?;
        self.query_sanitizer.sanitize(query)?;
        Ok(())
    }
}