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

impl ErrorCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCategory::Io => "io",
            ErrorCategory::Connection => "connection",
            ErrorCategory::Processing => "processing",
            ErrorCategory::Timeout => "timeout",
            ErrorCategory::Backpressure => "backpressure",
            ErrorCategory::Session => "session",
            ErrorCategory::Crypto => "crypto",
            ErrorCategory::CircuitBreaker => "circuit_breaker",
            ErrorCategory::Unknown => "unknown",
        }
    }

    /// Базовый вес ошибки (для приоритизации)
    pub fn base_weight(&self) -> f64 {
        match self {
            ErrorCategory::CircuitBreaker => 1.0,
            ErrorCategory::Crypto => 0.9,
            ErrorCategory::Session => 0.8,
            ErrorCategory::Connection => 0.7,
            ErrorCategory::Timeout => 0.6,
            ErrorCategory::Processing => 0.5,
            ErrorCategory::Io => 0.4,
            ErrorCategory::Backpressure => 0.3,
            ErrorCategory::Unknown => 0.2,
        }
    }

    /// Критичность ошибки (0-1)
    pub fn severity(&self) -> f64 {
        match self {
            ErrorCategory::CircuitBreaker => 0.9,
            ErrorCategory::Crypto => 0.8,
            ErrorCategory::Session => 0.7,
            ErrorCategory::Connection => 0.6,
            ErrorCategory::Timeout => 0.5,
            ErrorCategory::Processing => 0.4,
            ErrorCategory::Io => 0.3,
            ErrorCategory::Backpressure => 0.2,
            ErrorCategory::Unknown => 0.1,
        }
    }
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

    /// Обновление интенсивности
    pub fn update(&mut self, now: Instant) {
        // Очистка старых записей (> 60 секунд)
        while let Some(&ts) = self.timestamps.front() {
            if now.duration_since(ts) > Duration::from_secs(60) {
                self.timestamps.pop_front();
            } else {
                break;
            }
        }

        let window = 60.0;
        let count = self.timestamps.len();
        self.lambda = count as f64 / window;
        self.peak_lambda = self.peak_lambda.max(self.lambda);

        // EMA для среднего
        let alpha = 0.1;
        self.avg_lambda = self.avg_lambda * (1.0 - alpha) + self.lambda * alpha;

        // CV
        if !self.timestamps.is_empty() {
            let mut intervals = Vec::new();
            let mut prev = self.timestamps.front().unwrap();
            for ts in self.timestamps.iter().skip(1) {
                intervals.push(ts.duration_since(*prev).as_secs_f64());
                prev = ts;
            }

            if !intervals.is_empty() {
                let mean = intervals.iter().sum::<f64>() / intervals.len() as f64;
                let variance = intervals.iter()
                    .map(|&x| (x - mean).powi(2))
                    .sum::<f64>() / intervals.len() as f64;
                self.cv = variance.sqrt() / mean.max(0.001);
            }
        }
    }

    /// Добавление ошибки
    pub fn add_error(&mut self, category: ErrorCategory, timestamp: Instant) {
        self.timestamps.push_back(timestamp);

        if let Some(history) = self.history.get_mut(&category) {
            history.push_back(self.lambda);
            if history.len() > self.max_history {
                history.pop_front();
            }
        }
    }

    /// Вероятность ошибки в следующий момент
    pub fn error_probability(&self, dt: Duration) -> f64 {
        1.0 - (-self.lambda * dt.as_secs_f64()).exp()
    }

    /// Интенсивность ошибок по категории
    pub fn category_rate(&self, category: &ErrorCategory) -> f64 {
        self.history
            .get(category)
            .and_then(|h| h.back())
            .copied()
            .unwrap_or(0.0)
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

    /// Добавление ошибки и обновление корреляций
    pub fn add_error(&mut self, category: ErrorCategory, timestamp: Instant) {
        self.error_history.push_back((category.clone(), timestamp));

        // Очистка старых записей
        while let Some((_, ts)) = self.error_history.front() {
            if timestamp.duration_since(*ts) > self.correlation_window {
                self.error_history.pop_front();
            } else {
                break;
            }
        }

        self.update_correlations();
    }

    /// Обновление корреляционной матрицы
    fn update_correlations(&mut self) {
        let mut counts: HashMap<ErrorCategory, HashMap<ErrorCategory, usize>> = HashMap::new();
        let mut totals: HashMap<ErrorCategory, usize> = HashMap::new();

        // Подсчёт совместных появлений в окне
        for i in 0..self.error_history.len() {
            let (cat1, ts1) = &self.error_history[i];

            *totals.entry(cat1.clone()).or_insert(0) += 1;

            for j in i + 1..self.error_history.len() {
                let (cat2, ts2) = &self.error_history[j];

                // Исправление: используем saturating_duration_since для получения абсолютной разницы
                let time_diff = if ts1 > ts2 {
                    ts1.saturating_duration_since(*ts2)
                } else {
                    ts2.saturating_duration_since(*ts1)
                };

                if time_diff <= self.correlation_window {
                    let entry = counts
                        .entry(cat1.clone())
                        .or_insert_with(HashMap::new)
                        .entry(cat2.clone())
                        .or_insert(0);
                    *entry += 1;

                    let entry = counts
                        .entry(cat2.clone())
                        .or_insert_with(HashMap::new)
                        .entry(cat1.clone())
                        .or_insert(0);
                    *entry += 1;
                }
            }
        }

        // Расчёт корреляции Пирсона
        for (cat1, cat1_counts) in &counts {
            let corr_map = self.correlation_matrix
                .entry(cat1.clone())
                .or_insert_with(HashMap::new);

            for (cat2, count) in cat1_counts {
                let n11 = *count as f64;
                let n1_ = *totals.get(cat1).unwrap_or(&0) as f64;
                let n_1 = *totals.get(cat2).unwrap_or(&0) as f64;
                let n = self.error_history.len() as f64;

                // Коэффициент корреляции
                let corr = (n11 * n - n1_ * n_1) /
                    ((n1_ * (n - n1_) * n_1 * (n - n_1)).sqrt() + 0.001);

                corr_map.insert(cat2.clone(), corr.clamp(-1.0, 1.0));
            }
        }
    }

    /// Вероятность ошибки категории B после ошибки категории A
    pub fn conditional_probability(&self, cat_a: &ErrorCategory, cat_b: &ErrorCategory) -> f64 {
        self.correlation_matrix
            .get(cat_a)
            .and_then(|m| m.get(cat_b))
            .copied()
            .unwrap_or(0.0)
            .max(0.0)
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

impl BatchError {
    /// Категория ошибки
    pub fn category(&self) -> ErrorCategory {
        match self {
            BatchError::Io(_) => ErrorCategory::Io,
            BatchError::ConnectionError(_) => ErrorCategory::Connection,
            BatchError::ConnectionClosed(_) => ErrorCategory::Connection,
            BatchError::ProcessingError(_) => ErrorCategory::Processing,
            BatchError::Timeout => ErrorCategory::Timeout,
            BatchError::Backpressure => ErrorCategory::Backpressure,
            BatchError::InvalidSession(_) => ErrorCategory::Session,
            BatchError::Crypto(_) => ErrorCategory::Crypto,
            BatchError::CircuitBreakerOpen(_) => ErrorCategory::CircuitBreaker,
        }
    }

    /// Краткое описание
    pub fn brief(&self) -> &'static str {
        match self {
            BatchError::Io(_) => "IO operation failed",
            BatchError::ConnectionError(_) => "Connection error",
            BatchError::ConnectionClosed(_) => "Connection closed",
            BatchError::ProcessingError(_) => "Processing failed",
            BatchError::Timeout => "Operation timeout",
            BatchError::Backpressure => "Backpressure",
            BatchError::InvalidSession(_) => "Invalid session",
            BatchError::Crypto(_) => "Cryptography error",
            BatchError::CircuitBreakerOpen(_) => "Circuit breaker open",
        }
    }

    /// Восстанавливаемая ли ошибка
    pub fn is_recoverable(&self) -> bool {
        match self {
            BatchError::Timeout => true,
            BatchError::Backpressure => true,
            BatchError::ConnectionError(_) => true,
            BatchError::Io(_) => true,
            BatchError::CircuitBreakerOpen(_) => true,
            _ => false,
        }
    }

    /// Требует ли ошибка немедленного вмешательства
    pub fn is_critical(&self) -> bool {
        match self {
            BatchError::Crypto(_) => true,
            BatchError::InvalidSession(_) => true,
            BatchError::ConnectionClosed(_) => true,
            _ => false,
        }
    }
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