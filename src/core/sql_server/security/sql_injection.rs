use regex::Regex;
use super::error::SecurityError;

pub struct SqlInjectionDetector {
    malicious_patterns: Vec<Regex>,
    whitelist_patterns: Vec<Regex>,
    suspicious_keywords: Vec<&'static str>,
}

impl SqlInjectionDetector {
    pub fn new() -> Self {
        let malicious_patterns = vec![
            Regex::new(r"(?i)(union\s+select)").unwrap(),
            Regex::new(r"(?i)(drop\s+table)").unwrap(),
            Regex::new(r"(?i)(insert\s+into)").unwrap(),
            Regex::new(r"(?i)(delete\s+from)").unwrap(),
            Regex::new(r"(?i)(--|\#)").unwrap(),
            Regex::new(r"(?i)(or\s+1=1)").unwrap(),
            Regex::new(r"(?i)(;\s*DROP)").unwrap(),
        ];

        let whitelist_patterns = vec![
            Regex::new(r"^SELECT\s+[\w\s,.*]+\s+FROM\s+\w+").unwrap(),
            Regex::new(r"^INSERT\s+INTO\s+\w+\s+\([^)]+\)\s+VALUES\s*\([^)]+\)").unwrap(),
            Regex::new(r"^UPDATE\s+\w+\s+SET\s+[\w\s,=]+\s+WHERE").unwrap(),
            Regex::new(r"^DELETE\s+FROM\s+\w+\s+WHERE").unwrap(),
        ];

        let suspicious_keywords = vec![
            "DROP", "TRUNCATE", "SHUTDOWN", "EXEC", "EXECUTE", "xp_", "sp_",
        ];

        Self {
            malicious_patterns,
            whitelist_patterns,
            suspicious_keywords,
        }
    }

    pub fn detect(&self, query: &str) -> Result<(), SecurityError> {
        // Сначала проверяем белый список
        for pattern in &self.whitelist_patterns {
            if pattern.is_match(query) {
                return Ok(());
            }
        }

        // Затем проверяем опасные паттерны
        for pattern in &self.malicious_patterns {
            if pattern.is_match(query) {
                return Err(SecurityError::SqlInjectionAttempt);
            }
        }

        let upper_query = query.to_uppercase();
        for keyword in &self.suspicious_keywords {
            if upper_query.contains(keyword) {
                return Err(SecurityError::SqlInjectionAttempt);
            }
        }

        if query.contains(';') && query.matches(';').count() > 1 {
            return Err(SecurityError::SqlInjectionAttempt);
        }

        Ok(())
    }

    pub fn get_detector_info(&self) -> DetectorInfo {
        DetectorInfo {
            malicious_patterns_count: self.malicious_patterns.len(),
            whitelist_patterns_count: self.whitelist_patterns.len(),
            suspicious_keywords_count: self.suspicious_keywords.len(),
        }
    }
}

pub struct DetectorInfo {
    pub malicious_patterns_count: usize,
    pub whitelist_patterns_count: usize,
    pub suspicious_keywords_count: usize,
}