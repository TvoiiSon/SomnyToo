use std::io;
use std::time::{Duration, Instant};
use std::collections::{HashMap, VecDeque};
use lazy_static::lazy_static;

/// Классификация ошибок с вероятностными моделями
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ErrorCategory {
    Io,
    Connection,
    Processing,
    Timeout,
    Backpressure,
    Session,
    Crypto,
    CircuitBreaker,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ErrorRateModel {
    /// Текущая интенсивность ошибок (λ)
    pub lambda: f64,
    /// Пиковая интенсивность
    pub peak_lambda: f64,
    /// Средняя интенсивность
    pub avg_lambda: f64,
    /// Коэффициент вариации
    pub cv: f64,
    /// История интенсивности по категориям
    pub history: HashMap<ErrorCategory, VecDeque<f64>>,
    /// Временные метки ошибок
    pub timestamps: VecDeque<Instant>,
    /// Максимальный размер истории
    pub max_history: usize,
}

impl ErrorRateModel {
    pub fn new(max_history: usize) -> Self {
        let mut history = HashMap::new();
        history.insert(ErrorCategory::Io, VecDeque::with_capacity(max_history));
        history.insert(ErrorCategory::Connection, VecDeque::with_capacity(max_history));
        history.insert(ErrorCategory::Processing, VecDeque::with_capacity(max_history));
        history.insert(ErrorCategory::Timeout, VecDeque::with_capacity(max_history));
        history.insert(ErrorCategory::Backpressure, VecDeque::with_capacity(max_history));
        history.insert(ErrorCategory::Session, VecDeque::with_capacity(max_history));
        history.insert(ErrorCategory::Crypto, VecDeque::with_capacity(max_history));
        history.insert(ErrorCategory::CircuitBreaker, VecDeque::with_capacity(max_history));
        history.insert(ErrorCategory::Unknown, VecDeque::with_capacity(max_history));

        Self {
            lambda: 0.0,
            peak_lambda: 0.0,
            avg_lambda: 0.0,
            cv: 0.0,
            history,
            timestamps: VecDeque::with_capacity(max_history),
            max_history,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ErrorCorrelationModel {
    /// Корреляционная матрица между категориями
    pub correlation_matrix: HashMap<ErrorCategory, HashMap<ErrorCategory, f64>>,
    /// Временные окна для корреляции
    pub correlation_window: Duration,
    /// История ошибок с временными метками
    pub error_history: VecDeque<(ErrorCategory, Instant)>,
}

impl ErrorCorrelationModel {
    pub fn new(window: Duration, max_history: usize) -> Self {
        Self {
            correlation_matrix: HashMap::new(),
            correlation_window: window,
            error_history: VecDeque::with_capacity(max_history),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

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

    #[error("Connection closed: {0}")]
    ConnectionClosed(String),

    #[error("Circuit breaker open: {0}")]
    CircuitBreakerOpen(String),
}

/// Результат batch операций
pub type BatchResult<T> = Result<T, BatchError>;

lazy_static! {
    static ref ERROR_RATE_MODEL: std::sync::RwLock<ErrorRateModel> =
        std::sync::RwLock::new(ErrorRateModel::new(1000));

    static ref ERROR_CORRELATION_MODEL: std::sync::RwLock<ErrorCorrelationModel> =
        std::sync::RwLock::new(ErrorCorrelationModel::new(
            Duration::from_secs(10),
            1000
        ));
}