use std::fmt;
use std::error::Error;

#[derive(Debug, Clone)]
pub enum SecurityError {
    RateLimitExceeded,
    SqlInjectionAttempt,
    QueryTooLong,
    QueryTooLarge,
    InvalidOperation,
    TableNotAllowed,
    InvalidQueryPattern,
    IpBlocked,
}

impl fmt::Display for SecurityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecurityError::RateLimitExceeded => write!(f, "Rate limit exceeded"),
            SecurityError::SqlInjectionAttempt => write!(f, "SQL injection attempt detected"),
            SecurityError::QueryTooLong => write!(f, "Query exceeds maximum allowed length"),
            SecurityError::QueryTooLarge => write!(f, "Query too large"),
            SecurityError::InvalidOperation => write!(f, "Invalid SQL operation"),
            SecurityError::TableNotAllowed => write!(f, "Access to table not allowed"),
            SecurityError::InvalidQueryPattern => write!(f, "Query pattern not allowed"),
            SecurityError::IpBlocked => write!(f, "IP address is blocked"),
        }
    }
}

impl Error for SecurityError {}