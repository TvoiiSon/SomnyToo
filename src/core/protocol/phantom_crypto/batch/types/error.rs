/// Единые ошибки batch системы
#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Connection error: {0}")]
    ConnectionError(String),

    #[error("Processing error: {0}")]
    ProcessingError(String),

    #[error("Timeout error")]
    Timeout,

    #[error("Backpressure: too many pending operations")]
    Backpressure,

    #[error("Invalid session: {0}")]
    InvalidSession(String),

    #[error("Crypto error: {0}")]
    Crypto(String),
}

/// Результат batch операций
pub type BatchResult<T> = Result<T, BatchError>;