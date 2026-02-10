use std::sync::Arc;
use std::time::{Instant, Duration};
use tokio::sync::RwLock;
use tracing::{info, warn};
use dashmap::DashMap;

/// Состояния Circuit Breaker
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CircuitState {
    Closed,     // Нормальная работа
    Open,       // Открыт - запросы блокируются
    HalfOpen,   // Пробный режим
}

/// Circuit Breaker для компонентов системы
pub struct CircuitBreaker {
    name: String,
    state: RwLock<CircuitState>,
    failure_count: RwLock<usize>,
    last_failure: RwLock<Option<Instant>>,
    failure_threshold: usize,
    recovery_timeout: Duration,
    half_open_max_requests: usize,
    half_open_success_count: RwLock<usize>,
    metrics: Arc<DashMap<String, MetricValue>>,
}

impl CircuitBreaker {
    pub fn new(
        name: String,
        failure_threshold: usize,
        recovery_timeout: Duration,
        half_open_max_requests: usize,
        metrics: Arc<DashMap<String, MetricValue>>,
    ) -> Self {
        Self {
            name,
            state: RwLock::new(CircuitState::Closed),
            failure_count: RwLock::new(0),
            last_failure: RwLock::new(None),
            failure_threshold,
            recovery_timeout,
            half_open_max_requests,
            half_open_success_count: RwLock::new(0),
            metrics,
        }
    }

    /// Проверка, можно ли выполнить операцию
    pub async fn allow_request(&self) -> bool {
        let state = *self.state.read().await;

        match state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Проверяем, не прошло ли достаточно времени для recovery
                if let Some(last_failure) = *self.last_failure.read().await {
                    if Instant::now().duration_since(last_failure) >= self.recovery_timeout {
                        // Переходим в HalfOpen
                        *self.state.write().await = CircuitState::HalfOpen;
                        *self.half_open_success_count.write().await = 0;
                        info!("🔧 Circuit breaker '{}' переход в HalfOpen", self.name);
                        return true;
                    }
                }
                false
            }
            CircuitState::HalfOpen => {
                let count = *self.half_open_success_count.read().await;
                count < self.half_open_max_requests
            }
        }
    }

    /// Отметить успешное выполнение
    pub async fn record_success(&self) {
        let mut state = self.state.write().await;

        match *state {
            CircuitState::HalfOpen => {
                let mut count = self.half_open_success_count.write().await;
                *count += 1;

                if *count >= self.half_open_max_requests {
                    // Восстановление успешно
                    *state = CircuitState::Closed;
                    *self.failure_count.write().await = 0;
                    *self.last_failure.write().await = None;
                    *count = 0;

                    info!("✅ Circuit breaker '{}' восстановлен", self.name);
                    self.record_metric("recovered".to_string(), 1.0);
                }
            }
            CircuitState::Closed => {
                // Сбрасываем счетчик ошибок после успешных операций
                *self.failure_count.write().await = 0;
            }
            _ => {}
        }
    }

    /// Отметить ошибку
    pub async fn record_failure(&self) {
        let mut failure_count = self.failure_count.write().await;
        *failure_count += 1;

        *self.last_failure.write().await = Some(Instant::now());

        // Обновляем метрики
        self.record_metric("failures".to_string(), *failure_count as f64);
        self.record_metric("failure_rate".to_string(),
                           *failure_count as f64 / self.failure_threshold as f64);

        if *failure_count >= self.failure_threshold {
            let mut state = self.state.write().await;
            if *state != CircuitState::Open {
                *state = CircuitState::Open;
                warn!("🚨 Circuit breaker '{}' открыт после {} ошибок",
                    self.name, *failure_count);
                self.record_metric("circuit_opened".to_string(), 1.0);
            }
        }
    }

    /// Принудительно сбросить
    pub async fn reset(&self) {
        *self.state.write().await = CircuitState::Closed;
        *self.failure_count.write().await = 0;
        *self.last_failure.write().await = None;
        *self.half_open_success_count.write().await = 0;

        info!("🔄 Circuit breaker '{}' принудительно сброшен", self.name);
        self.record_metric("manual_reset".to_string(), 1.0);
    }

    /// Получить текущее состояние
    pub async fn get_state(&self) -> CircuitState {
        *self.state.read().await
    }

    /// Получить статистику
    pub async fn get_stats(&self) -> CircuitBreakerStats {
        CircuitBreakerStats {
            name: self.name.clone(),
            state: *self.state.read().await,
            failure_count: *self.failure_count.read().await,
            last_failure: *self.last_failure.read().await,
            failure_threshold: self.failure_threshold,
            recovery_timeout: self.recovery_timeout,
        }
    }

    fn record_metric(&self, key: String, value: f64) {
        self.metrics.insert(
            format!("circuit_breaker.{}.{}", self.name, key),
            MetricValue::Float(value)
        );
    }
}

#[derive(Debug, Clone)]
pub struct CircuitBreakerStats {
    pub name: String,
    pub state: CircuitState,
    pub failure_count: usize,
    pub last_failure: Option<Instant>,
    pub failure_threshold: usize,
    pub recovery_timeout: Duration,
}

/// Manager для множества Circuit Breakers
pub struct CircuitBreakerManager {
    breakers: DashMap<String, Arc<CircuitBreaker>>,
    config: Arc<super::config::BatchConfig>,
    metrics: Arc<DashMap<String, MetricValue>>,
}

impl CircuitBreakerManager {
    pub fn new(config: Arc<super::config::BatchConfig>) -> Self {
        Self {
            breakers: DashMap::new(),
            config,
            metrics: Arc::new(DashMap::new()),
        }
    }

    pub fn get_or_create(&self, name: &str) -> Arc<CircuitBreaker> {
        self.breakers.entry(name.to_string()).or_insert_with(|| {
            Arc::new(CircuitBreaker::new(
                name.to_string(),
                self.config.failure_threshold,
                self.config.recovery_timeout,
                self.config.half_open_max_requests,
                self.metrics.clone(),
            ))
        }).clone()
    }

    pub async fn get_breaker(&self, name: &str) -> Option<Arc<CircuitBreaker>> {
        self.breakers.get(name).map(|b| b.clone())
    }

    pub async fn get_all_stats(&self) -> Vec<CircuitBreakerStats> {
        let mut stats = Vec::new();

        for entry in self.breakers.iter() {
            let breaker = entry.value();
            let breaker_stats = breaker.get_stats().await; // Уже есть .await
            stats.push(breaker_stats);
        }

        stats
    }

    pub fn get_all_breakers(&self) -> Vec<Arc<CircuitBreaker>> {
        self.breakers.iter().map(|e| e.value().clone()).collect()
    }
}

#[derive(Debug, Clone)]
pub enum MetricValue {
    Integer(i64),
    Float(f64),
    Duration(Duration),
    String(String),
    Boolean(bool),
}