use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Instant, Duration};
use tokio::sync::RwLock;
use tracing::{info, warn};
use dashmap::DashMap;

/// Модель марковского процесса для состояний Circuit Breaker
#[derive(Debug, Clone)]
pub struct CircuitBreakerMarkovModel {
    /// Матрица интенсивностей переходов
    /// Q = [q_cc, q_co, q_ch; q_oc, q_oo, q_oh; q_hc, q_ho, q_hh]
    pub transition_rates: [[f64; 3]; 3],

    /// Стационарные вероятности состояний
    pub steady_state: [f64; 3],

    /// Среднее время до отказа (MTTF)
    pub mttf: f64,

    /// Среднее время восстановления (MTTR)
    pub mttr: f64,

    /// Коэффициент готовности
    pub availability: f64,
}

impl CircuitBreakerMarkovModel {
    pub fn new() -> Self {
        Self {
            transition_rates: [
                [0.0, 0.1, 0.01],  // Closed
                [0.05, 0.0, 0.1],  // Open
                [0.5, 0.05, 0.0],  // HalfOpen
            ],
            steady_state: [1.0, 0.0, 0.0],
            mttf: 1000.0,
            mttr: 10.0,
            availability: 0.99,  // БЫЛО: 0.3333, СТАЛО: 0.99
        }
    }

    /// Расчёт стационарных вероятностей решением системы уравнений
    /// π·Q = 0, Σπ = 1
    pub fn compute_steady_state(&mut self) {
        let q11 = -self.transition_rates[0][1] - self.transition_rates[0][2];
        let q22 = -self.transition_rates[1][0] - self.transition_rates[1][2];
        let q33 = -self.transition_rates[2][0] - self.transition_rates[2][1];

        // Система линейных уравнений
        let a = [
            [q11, self.transition_rates[1][0], self.transition_rates[2][0]],
            [self.transition_rates[0][1], q22, self.transition_rates[2][1]],
            [self.transition_rates[0][2], self.transition_rates[1][2], q33],
        ];

        let b = [0.0, 0.0, 1.0];

        // Решение методом Крамера
        let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);

        if det.abs() > 1e-10 {
            let det1 = b[0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
                - a[0][1] * (b[1] * a[2][2] - a[1][2] * b[2])
                + a[0][2] * (b[1] * a[2][1] - a[1][1] * b[2]);

            let det2 = a[0][0] * (b[1] * a[2][2] - a[1][2] * b[2])
                - b[0] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
                + a[0][2] * (a[1][0] * b[2] - b[1] * a[2][0]);

            let det3 = a[0][0] * (a[1][1] * b[2] - b[1] * a[2][1])
                - a[0][1] * (a[1][0] * b[2] - b[1] * a[2][0])
                + b[0] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);

            self.steady_state[0] = det1 / det;
            self.steady_state[1] = det2 / det;
            self.steady_state[2] = det3 / det;
        }

        // MTTF = 1 / λ, где λ - интенсивность перехода в Open
        self.mttf = 1.0 / self.transition_rates[0][1].max(0.001);

        // MTTR = 1 / μ, где μ - интенсивность восстановления
        self.mttr = 1.0 / self.transition_rates[1][0].max(0.001);

        // A = MTTF / (MTTF + MTTR)
        self.availability = self.mttf / (self.mttf + self.mttr);

        // Корректировка на half-open состояние (небольшой бонус)
        let half_open_bonus = self.steady_state[2] * 0.1;
        self.availability = (self.availability + half_open_bonus).min(1.0);

        // Нормировка
        self.availability = self.availability.clamp(0.0, 1.0);
    }
}

#[derive(Debug, Clone)]
pub struct FailureRateModel {
    /// Текущая оценка интенсивности отказов (λ)
    pub lambda: f64,

    /// Коэффициент сглаживания (α)
    pub alpha: f64,

    /// Пороговое значение для открытия цепи
    pub threshold: f64,

    /// Время полужизни для адаптации
    pub half_life: Duration,

    /// История интенсивности отказов
    pub history: VecDeque<f64>,
}

impl FailureRateModel {
    pub fn new(threshold: f64, half_life: Duration) -> Self {
        Self {
            lambda: 0.0,
            alpha: 0.1,
            threshold,
            half_life,
            history: VecDeque::with_capacity(100),
        }
    }

    /// Обновление оценки экспоненциальным сглаживанием
    pub fn update(&mut self, failure_occurred: bool) {
        let measurement = if failure_occurred { 1.0 } else { 0.0 };

        // Экспоненциальное сглаживание
        self.lambda = self.alpha * measurement + (1.0 - self.alpha) * self.lambda;

        // Адаптация alpha на основе half-life
        let half_life_secs = self.half_life.as_secs_f64();
        self.alpha = 1.0 - (-1.0 / half_life_secs).exp();

        self.history.push_back(self.lambda);
        if self.history.len() > 100 {
            self.history.pop_front();
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecoveryModel {
    /// Базовая задержка восстановления
    pub base_delay: Duration,

    /// Максимальная задержка
    pub max_delay: Duration,

    /// Коэффициент экспоненциального роста
    pub backoff_factor: f64,

    /// Текущая задержка
    pub current_delay: Duration,

    /// Количество попыток восстановления
    pub recovery_attempts: u32,
}

impl RecoveryModel {
    pub fn new(base_delay: Duration, max_delay: Duration, backoff_factor: f64) -> Self {
        Self {
            base_delay,
            max_delay,
            backoff_factor,
            current_delay: base_delay,
            recovery_attempts: 0,
        }
    }

    /// Расчёт следующей задержки: D = min(D_max, D_base * k^n)
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.base_delay.as_secs_f64() *
            self.backoff_factor.powi(self.recovery_attempts as i32);

        self.current_delay = Duration::from_secs_f64(
            delay.min(self.max_delay.as_secs_f64())
        );

        self.recovery_attempts += 1;
        self.current_delay
    }

    /// Сброс после успешного восстановления
    pub fn reset(&mut self) {
        self.current_delay = self.base_delay;
        self.recovery_attempts = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CircuitState {
    Closed = 0,     // Нормальная работа
    Open = 1,       // Открыт - запросы блокируются
    HalfOpen = 2,   // Пробный режим
}

#[derive(Debug, Clone)]
pub struct CircuitBreakerStats {
    pub name: String,
    pub state: CircuitState,
    pub failure_count: usize,
    pub success_count: usize,
    pub total_requests: usize,
    pub failure_rate: f64,
    pub success_rate: f64,
    pub last_failure: Option<Instant>,
    pub last_success: Option<Instant>,
    pub failure_threshold: usize,
    pub recovery_timeout: Duration,
    pub current_delay: Duration,
    pub recovery_attempts: u32,
    pub mttf: f64,
    pub mttr: f64,
    pub availability: f64,
    pub opened_count: u64,
    pub closed_count: u64,
    pub half_open_count: u64,
    pub uptime: Duration,
}

pub struct CircuitBreaker {
    name: String,
    pub markov_model: RwLock<CircuitBreakerMarkovModel>,
    failure_model: RwLock<FailureRateModel>,
    pub recovery_model: RwLock<RecoveryModel>,
    state: RwLock<CircuitState>,
    failure_count: RwLock<usize>,
    success_count: RwLock<usize>,
    total_requests: RwLock<usize>,
    last_failure: RwLock<Option<Instant>>,
    last_success: RwLock<Option<Instant>>,
    state_change_time: RwLock<Instant>,
    pub failure_threshold: usize,
    recovery_timeout: Duration,
    half_open_max_requests: usize,
    half_open_success_count: RwLock<usize>,
    consecutive_successes_needed: usize,
    opened_count: RwLock<u64>,
    closed_count: RwLock<u64>,
    half_open_count: RwLock<u64>,
    metrics: Arc<DashMap<String, MetricValue>>,
    failure_history: RwLock<VecDeque<FailureRecord>>,
}

#[derive(Debug, Clone)]
pub struct FailureRecord {
    pub timestamp: Instant,
    pub state: CircuitState,
    pub failure_count: usize,
    pub error: Option<String>,
}

impl CircuitBreaker {
    pub fn new(
        name: String,
        failure_threshold: usize,
        recovery_timeout: Duration,
        half_open_max_requests: usize,
        metrics: Arc<DashMap<String, MetricValue>>,
    ) -> Self {
        info!("🛡️ Initializing Mathematical CircuitBreaker v2.0: {}", name);

        let mut markov_model = CircuitBreakerMarkovModel::new();
        markov_model.compute_steady_state();

        let failure_model = FailureRateModel::new(
            failure_threshold as f64 / 100.0,
            Duration::from_secs(60)
        );

        let recovery_model = RecoveryModel::new(
            recovery_timeout,
            recovery_timeout * 16,
            2.0
        );

        info!("  Failure threshold: {}", failure_threshold);
        info!("  Recovery timeout: {:?}", recovery_timeout);
        info!("  Half-open max requests: {}", half_open_max_requests);
        info!("  Markov steady state: Closed={:.2}%, Open={:.2}%, HalfOpen={:.2}%",
              markov_model.steady_state[0] * 100.0,
              markov_model.steady_state[1] * 100.0,
              markov_model.steady_state[2] * 100.0);
        info!("  Availability: {:.4}%", markov_model.availability * 100.0);

        Self {
            name,
            markov_model: RwLock::new(markov_model),
            failure_model: RwLock::new(failure_model),
            recovery_model: RwLock::new(recovery_model),
            state: RwLock::new(CircuitState::Closed),
            failure_count: RwLock::new(0),
            success_count: RwLock::new(0),
            total_requests: RwLock::new(0),
            last_failure: RwLock::new(None),
            last_success: RwLock::new(None),
            state_change_time: RwLock::new(Instant::now()),
            failure_threshold,
            recovery_timeout,
            half_open_max_requests,
            half_open_success_count: RwLock::new(0),
            consecutive_successes_needed: half_open_max_requests,
            opened_count: RwLock::new(0),
            closed_count: RwLock::new(1),
            half_open_count: RwLock::new(0),
            metrics,
            failure_history: RwLock::new(VecDeque::with_capacity(100)),
        }
    }

    pub async fn allow_request(&self) -> bool {
        let state = *self.state.read().await;
        let mut total_requests = self.total_requests.write().await;
        *total_requests += 1;

        match state {
            CircuitState::Closed => {
                self.record_metric("allowed", 1.0);
                true
            }

            CircuitState::Open => {
                let should_attempt = self.should_attempt_recovery().await;

                if should_attempt {
                    // Переход в HalfOpen
                    let mut state = self.state.write().await;
                    *state = CircuitState::HalfOpen;
                    *self.half_open_success_count.write().await = 0;

                    let mut state_change_time = self.state_change_time.write().await;
                    *state_change_time = Instant::now();

                    let mut half_open_count = self.half_open_count.write().await;
                    *half_open_count += 1;

                    info!("🔄 Circuit breaker '{}' transitioning to HalfOpen (recovery attempt)",
                          self.name);

                    self.record_metric("transition.half_open", 1.0);
                    self.record_metric("recovery_attempts", 1.0);

                    true
                } else {
                    self.record_metric("rejected.open", 1.0);
                    false
                }
            }

            CircuitState::HalfOpen => {
                let success_count = *self.half_open_success_count.read().await;
                let allowed = success_count < self.half_open_max_requests;

                if allowed {
                    self.record_metric("allowed.half_open", 1.0);
                } else {
                    self.record_metric("rejected.half_open", 1.0);
                }

                allowed
            }
        }
    }

    async fn should_attempt_recovery(&self) -> bool {
        if let Some(last_failure) = *self.last_failure.read().await {
            let elapsed = Instant::now().duration_since(last_failure);

            // Получаем текущую задержку из модели восстановления
            let recovery_model = self.recovery_model.read().await;
            let current_delay = recovery_model.current_delay;
            drop(recovery_model);

            elapsed >= current_delay
        } else {
            false
        }
    }

    pub async fn record_success(&self) {
        let mut state = self.state.write().await;
        let mut success_count = self.success_count.write().await;
        *success_count += 1;

        *self.last_success.write().await = Some(Instant::now());

        self.record_metric("successes", 1.0);

        match *state {
            CircuitState::HalfOpen => {
                let mut half_open_success = self.half_open_success_count.write().await;
                *half_open_success += 1;

                self.record_metric("half_open.successes", 1.0);

                if *half_open_success >= self.consecutive_successes_needed {
                    // Полное восстановление - переход в Closed
                    *state = CircuitState::Closed;
                    *self.failure_count.write().await = 0;
                    *self.last_failure.write().await = None;
                    *half_open_success = 0;

                    let mut state_change_time = self.state_change_time.write().await;
                    *state_change_time = Instant::now();

                    let mut closed_count = self.closed_count.write().await;
                    *closed_count += 1;

                    // Сброс модели восстановления
                    let mut recovery_model = self.recovery_model.write().await;
                    recovery_model.reset();

                    // Обновление марковской модели
                    let mut markov = self.markov_model.write().await;
                    markov.transition_rates[1][0] *= 1.1; // Увеличиваем интенсивность восстановления
                    markov.compute_steady_state();

                    info!("✅ Circuit breaker '{}' fully recovered and closed", self.name);
                    self.record_metric("recovered", 1.0);
                    self.record_metric("state.closed", 1.0);
                }
            }

            CircuitState::Closed => {
                // Сброс счётчика отказов при успехе в Closed состоянии
                *self.failure_count.write().await = 0;

                // Обновление модели отказов
                let mut failure_model = self.failure_model.write().await;
                failure_model.update(false);
            }

            _ => {}
        }
    }

    pub async fn record_failure(&self) {
        let mut failure_count = self.failure_count.write().await;
        *failure_count += 1;

        *self.last_failure.write().await = Some(Instant::now());

        let current_failure_count = *failure_count;
        let state = *self.state.read().await;

        // Запись в историю
        let mut history = self.failure_history.write().await;
        history.push_back(FailureRecord {
            timestamp: Instant::now(),
            state,
            failure_count: current_failure_count,
            error: None,
        });
        if history.len() > 100 {
            history.pop_front();
        }

        self.record_metric("failures", 1.0);
        self.record_metric("failure_count", current_failure_count as f64);
        self.record_metric("failure_rate",
                           current_failure_count as f64 / self.failure_threshold as f64);

        // Обновление модели отказов
        {
            let mut failure_model = self.failure_model.write().await;
            failure_model.update(true);

            self.record_metric("failure_rate.lambda", failure_model.lambda);
        }

        match state {
            CircuitState::Closed => {
                // Проверка порога для открытия цепи
                if current_failure_count >= self.failure_threshold {
                    let mut state = self.state.write().await;
                    *state = CircuitState::Open;

                    let mut state_change_time = self.state_change_time.write().await;
                    *state_change_time = Instant::now();

                    let mut opened_count = self.opened_count.write().await;
                    *opened_count += 1;

                    // Экспоненциальное увеличение задержки восстановления
                    let mut recovery_model = self.recovery_model.write().await;
                    let next_delay = recovery_model.next_delay();

                    // Обновление марковской модели
                    let mut markov = self.markov_model.write().await;
                    markov.transition_rates[0][1] *= 1.1; // Увеличиваем интенсивность отказов
                    markov.compute_steady_state();

                    warn!("🚨 Circuit breaker '{}' opened after {} failures (MTTF={:.1}s, delay={:?})",
                          self.name, current_failure_count, markov.mttf, next_delay);

                    self.record_metric("circuit_opened", 1.0);
                    self.record_metric("state.open", 1.0);
                    self.record_metric("recovery_delay_ms", next_delay.as_millis() as f64);
                }
            }

            CircuitState::HalfOpen => {
                // Неудача в HalfOpen - немедленно возвращаемся в Open
                let mut state = self.state.write().await;
                *state = CircuitState::Open;

                let mut state_change_time = self.state_change_time.write().await;
                *state_change_time = Instant::now();

                let mut half_open_success = self.half_open_success_count.write().await;
                *half_open_success = 0;

                // Увеличиваем задержку восстановления
                let mut recovery_model = self.recovery_model.write().await;
                let next_delay = recovery_model.next_delay();

                warn!("⚠️ Circuit breaker '{}' failed in HalfOpen, returning to Open (delay={:?})",
                      self.name, next_delay);

                self.record_metric("half_open.failure", 1.0);
                self.record_metric("state.open", 1.0);
            }

            _ => {}
        }
    }

    pub async fn reset(&self) {
        *self.state.write().await = CircuitState::Closed;
        *self.failure_count.write().await = 0;
        *self.success_count.write().await = 0;
        *self.last_failure.write().await = None;
        *self.last_success.write().await = None;
        *self.half_open_success_count.write().await = 0;

        let mut state_change_time = self.state_change_time.write().await;
        *state_change_time = Instant::now();

        let mut closed_count = self.closed_count.write().await;
        *closed_count += 1;

        // Сброс моделей
        {
            let mut failure_model = self.failure_model.write().await;
            failure_model.lambda = 0.0;
            failure_model.history.clear();
        }

        {
            let mut recovery_model = self.recovery_model.write().await;
            recovery_model.reset();
        }

        {
            let mut markov = self.markov_model.write().await;
            markov.transition_rates = [
                [0.0, 0.1, 0.01],
                [0.05, 0.0, 0.1],
                [0.5, 0.05, 0.0],
            ];
            markov.compute_steady_state();
        }

        info!("🔄 Circuit breaker '{}' manually reset", self.name);
        self.record_metric("manual_reset", 1.0);
        self.record_metric("state.closed", 1.0);
    }

    pub async fn get_state(&self) -> CircuitState {
        *self.state.read().await
    }

    pub async fn get_stats(&self) -> CircuitBreakerStats {
        let state = *self.state.read().await;
        let failure_count = *self.failure_count.read().await;
        let success_count = *self.success_count.read().await;
        let total_requests = *self.total_requests.read().await;
        let last_failure = *self.last_failure.read().await;
        let last_success = *self.last_success.read().await;
        let state_change_time = *self.state_change_time.read().await;
        let _half_open_success = *self.half_open_success_count.read().await;
        let opened_count = *self.opened_count.read().await;
        let closed_count = *self.closed_count.read().await;
        let half_open_count = *self.half_open_count.read().await;

        let failure_rate = if total_requests > 0 {
            failure_count as f64 / total_requests as f64
        } else { 0.0 };

        let success_rate = if total_requests > 0 {
            success_count as f64 / total_requests as f64
        } else { 0.0 };

        let recovery_model = self.recovery_model.read().await;
        let markov = self.markov_model.read().await;

        let uptime = if state == CircuitState::Closed {
            state_change_time.elapsed()
        } else {
            Duration::from_secs(0)
        };

        CircuitBreakerStats {
            name: self.name.clone(),
            state,
            failure_count,
            success_count,
            total_requests,
            failure_rate,
            success_rate,
            last_failure,
            last_success,
            failure_threshold: self.failure_threshold,
            recovery_timeout: self.recovery_timeout,
            current_delay: recovery_model.current_delay,
            recovery_attempts: recovery_model.recovery_attempts,
            mttf: markov.mttf,
            mttr: markov.mttr,
            availability: markov.availability,
            opened_count,
            closed_count,
            half_open_count,
            uptime,
        }
    }

    fn record_metric(&self, key: &str, value: f64) {
        self.metrics.insert(
            format!("circuit_breaker.{}.{}", self.name, key),
            MetricValue::Float(value)
        );
    }
}

#[derive(Debug, Clone)]
pub struct ReliabilityPrediction {
    pub time_horizon: Duration,
    pub current_state: CircuitState,
    pub probability_stay_in_state: f64,
    pub expected_failures: f64,
    pub reliability: f64,
    pub availability: f64,
}

pub struct CircuitBreakerManager {
    breakers: DashMap<String, Arc<CircuitBreaker>>,
    config: Arc<super::config::BatchConfig>,
    metrics: Arc<DashMap<String, MetricValue>>,
    _system_markov: RwLock<CircuitBreakerMarkovModel>,
    _global_failure_rate: RwLock<FailureRateModel>,
}

impl CircuitBreakerManager {
    pub fn new(config: Arc<super::config::BatchConfig>) -> Self {
        info!("🏢 Initializing CircuitBreakerManager");

        let _system_markov = RwLock::new(CircuitBreakerMarkovModel::new());
        let _global_failure_rate = RwLock::new(FailureRateModel::new(
            0.01,
            Duration::from_secs(300)
        ));

        Self {
            breakers: DashMap::new(),
            config,
            metrics: Arc::new(DashMap::new()),
            _system_markov,
            _global_failure_rate,
        }
    }

    pub fn get_or_create(&self, name: &str) -> Arc<CircuitBreaker> {
        self.breakers.entry(name.to_string()).or_insert_with(|| {
            info!("➕ Creating new circuit breaker: {}", name);

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
            let breaker_stats = breaker.get_stats().await;
            stats.push(breaker_stats);
        }

        stats
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

impl MetricValue {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            MetricValue::Integer(i) => Some(*i as f64),
            MetricValue::Float(f) => Some(*f),
            MetricValue::Duration(d) => Some(d.as_secs_f64()),
            MetricValue::Boolean(b) => Some(if *b { 1.0 } else { 0.0 }),
            MetricValue::String(_) => None,
        }
    }
}