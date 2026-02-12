use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore, SemaphorePermit};
use tracing::{info, warn};
use dashmap::DashMap;

use crate::core::protocol::batch_system::types::priority::Priority;

/// Модель обобщённого процессорного разделения (GPS - Generalized Processor Sharing)
#[derive(Debug, Clone)]
pub struct GPSModel {
    /// Веса приоритетов (φ_i)
    pub weights: [f64; 5],
    /// Доли пропускной способности (C_i)
    pub shares: [f64; 5],
    /// Нормированные веса
    pub normalized_weights: [f64; 5],
    /// Общая пропускная способность
    pub total_capacity: f64,
    /// Интенсивность поступления для каждого класса (λ_i)
    pub arrival_rates: [f64; 5],
    /// Интенсивность обслуживания для каждого класса (μ_i)
    pub service_rates: [f64; 5],
    /// Загрузка для каждого класса (ρ_i)
    pub utilizations: [f64; 5],
    /// Общая загрузка (ρ)
    pub total_utilization: f64,
}

impl GPSModel {
    pub fn new(total_capacity: f64) -> Self {
        Self {
            weights: [4.0, 2.0, 1.0, 0.5, 0.25],
            shares: [0.0; 5],
            normalized_weights: [0.0; 5],
            total_capacity,
            arrival_rates: [0.0; 5],
            service_rates: [1000.0; 5],
            utilizations: [0.0; 5],
            total_utilization: 0.0,
        }
    }

    /// Нормировка весов: φ_i' = φ_i / Σ φ_j
    pub fn normalize_weights(&mut self) {
        let sum: f64 = self.weights.iter().sum();
        for i in 0..5 {
            self.normalized_weights[i] = self.weights[i] / sum;
        }
    }

    /// Расчёт долей пропускной способности: C_i = φ_i' * C_total
    pub fn compute_shares(&mut self) {
        self.normalize_weights();
        for i in 0..5 {
            self.shares[i] = self.normalized_weights[i] * self.total_capacity;
        }
    }

    /// Расчёт загрузки: ρ_i = λ_i * E[X] / C_i
    pub fn compute_utilization(&mut self, batch_size: f64) {
        self.total_utilization = 0.0;
        for i in 0..5 {
            if self.shares[i] > 0.0 {
                self.utilizations[i] = self.arrival_rates[i] * batch_size / self.shares[i];
            } else {
                self.utilizations[i] = 0.0;
            }
            self.total_utilization += self.utilizations[i];
        }
    }

    /// Пропускная способность для класса i
    pub fn throughput(&self, class: usize) -> f64 {
        if class >= 5 {
            return 0.0;
        }
        self.shares[class] * (1.0 - self.utilizations[class])
    }
}

#[derive(Debug, Clone)]
pub struct WFQModel {
    /// Виртуальное время
    pub virtual_time: f64,
    /// Время завершения для каждого пакета
    pub finish_times: Vec<Vec<f64>>,
    /// Очереди по приоритетам
    pub queues: Vec<Vec<f64>>,
    /// Веса очередей
    pub weights: [f64; 5],
}

impl WFQModel {
    pub fn new() -> Self {
        Self {
            virtual_time: 0.0,
            finish_times: vec![Vec::new(); 5],
            queues: vec![Vec::new(); 5],
            weights: [4.0, 2.0, 1.0, 0.5, 0.25],
        }
    }

    /// Время начала обслуживания пакета
    pub fn start_time(&self, _packet_length: f64, class: usize) -> f64 {
        if class >= 5 {
            return self.virtual_time;
        }

        self.virtual_time.max(
            self.finish_times[class]
                .last()
                .copied()
                .unwrap_or(0.0)
        )
    }

    /// Время завершения обслуживания пакета
    pub fn finish_time(&self, packet_length: f64, class: usize) -> f64 {
        let start = self.start_time(packet_length, class);
        start + packet_length / self.weights[class]
    }
}

#[derive(Debug, Clone)]
pub struct TokenBucket {
    /// Скорость пополнения токенов (r)
    pub rate: f64,
    /// Максимальный размер бакета (b)
    pub capacity: f64,
    /// Текущее количество токенов
    pub tokens: f64,
    /// Время последнего обновления
    pub last_update: Instant,
}

impl TokenBucket {
    pub fn new(rate: f64, capacity: f64) -> Self {
        Self {
            rate,
            capacity,
            tokens: capacity,
            last_update: Instant::now(),
        }
    }

    /// Обновление токенов
    pub fn update(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        self.last_update = now;
    }

    /// Попытка изъять токены
    pub fn try_consume(&mut self, tokens: f64) -> bool {
        self.update();

        if self.tokens >= tokens {
            self.tokens -= tokens;
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone)]
pub struct LeakyBucket {
    /// Скорость утечки (r)
    pub rate: f64,
    /// Максимальный размер бакета (b)
    pub capacity: f64,
    /// Текущий уровень воды
    pub level: f64,
    /// Время последнего обновления
    pub last_update: Instant,
}

impl LeakyBucket {
    pub fn new(rate: f64, capacity: f64) -> Self {
        Self {
            rate,
            capacity,
            level: 0.0,
            last_update: Instant::now(),
        }
    }

    /// Обновление уровня
    pub fn update(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.level = (self.level - elapsed * self.rate).max(0.0);
        self.last_update = now;
    }

    /// Попытка добавить воду
    pub fn try_add(&mut self, amount: f64) -> bool {
        self.update();

        if self.level + amount <= self.capacity {
            self.level += amount;
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone)]
pub struct QosQuotas {
    /// Базовые квоты (нормированные)
    pub base_high_priority: f64,
    pub base_normal_priority: f64,
    pub base_low_priority: f64,

    /// Текущие квоты
    pub current_high_priority: f64,
    pub current_normal_priority: f64,
    pub current_low_priority: f64,

    /// Общая ёмкость системы
    pub total_capacity: usize,

    /// Время последней адаптации
    pub last_adaptation: Instant,

    /// Модель GPS
    pub gps_model: GPSModel,

    /// Token buckets для rate limiting
    pub high_token_bucket: TokenBucket,
    pub normal_token_bucket: TokenBucket,
    pub low_token_bucket: TokenBucket,

    /// Leaky buckets для сглаживания
    pub high_leaky_bucket: LeakyBucket,
    pub normal_leaky_bucket: LeakyBucket,
    pub low_leaky_bucket: LeakyBucket,
}

impl QosQuotas {
    pub fn new(high_quota: f64, normal_quota: f64, low_quota: f64, total_capacity: usize) -> Self {
        let total = high_quota + normal_quota + low_quota;
        let (norm_high, norm_normal, norm_low) = if (total - 1.0).abs() > 0.01 {
            warn!("⚠️ QoS quotas don't sum to 1.0 ({}), normalizing", total);
            (
                high_quota / total,
                normal_quota / total,
                low_quota / total,
            )
        } else {
            (high_quota, normal_quota, low_quota)
        };

        let high_capacity = (total_capacity as f64 * norm_high).ceil() as usize * 10;
        let normal_capacity = (total_capacity as f64 * norm_normal).ceil() as usize * 10;
        let low_capacity = (total_capacity as f64 * norm_low).ceil() as usize * 10;

        let mut gps_model = GPSModel::new(total_capacity as f64);
        gps_model.weights = [4.0, 2.0, 1.0, 0.5, 0.25];
        gps_model.compute_shares();

        Self {
            base_high_priority: norm_high,
            base_normal_priority: norm_normal,
            base_low_priority: norm_low,
            current_high_priority: norm_high,
            current_normal_priority: norm_normal,
            current_low_priority: norm_low,
            total_capacity,
            last_adaptation: Instant::now(),
            gps_model,
            high_token_bucket: TokenBucket::new(high_capacity as f64 / 1000.0, high_capacity as f64),
            normal_token_bucket: TokenBucket::new(normal_capacity as f64 / 1000.0, normal_capacity as f64),
            low_token_bucket: TokenBucket::new(low_capacity as f64 / 1000.0, low_capacity as f64),
            high_leaky_bucket: LeakyBucket::new(
                high_capacity as f64 / 100.0,  // rate = 4 ops/ms = 4000 ops/sec
                high_capacity as f64           // capacity = 4000
            ),
            normal_leaky_bucket: LeakyBucket::new(
                normal_capacity as f64 / 100.0,
                normal_capacity as f64
            ),
            low_leaky_bucket: LeakyBucket::new(
                low_capacity as f64 / 100.0,
                low_capacity as f64
            ),
        }
    }

    /// Обновление моделей на основе текущей нагрузки
    pub fn update_models(&mut self, arrival_rates: [f64; 5], batch_size: f64) {
        self.gps_model.arrival_rates = arrival_rates;
        self.gps_model.compute_utilization(batch_size);
    }
}

#[derive(Debug, Clone)]
pub struct QosStatistics {
    pub high_priority_requests: u64,
    pub normal_priority_requests: u64,
    pub low_priority_requests: u64,
    pub high_priority_rejected: u64,
    pub normal_priority_rejected: u64,
    pub low_priority_rejected: u64,
    pub high_priority_avg_wait_ms: f64,
    pub normal_priority_avg_wait_ms: f64,
    pub low_priority_avg_wait_ms: f64,
    pub high_priority_avg_queue: f64,
    pub normal_priority_avg_queue: f64,
    pub low_priority_avg_queue: f64,
    pub high_priority_loss_prob: f64,
    pub normal_priority_loss_prob: f64,
    pub low_priority_loss_prob: f64,
    pub high_priority_throughput: f64,
    pub normal_priority_throughput: f64,
    pub low_priority_throughput: f64,
    pub gps_utilizations: [f64; 5],
    pub total_utilization: f64,
    pub adaptation_count: u64,
}

impl Default for QosStatistics {
    fn default() -> Self {
        Self {
            high_priority_requests: 0,
            normal_priority_requests: 0,
            low_priority_requests: 0,
            high_priority_rejected: 0,
            normal_priority_rejected: 0,
            low_priority_rejected: 0,
            high_priority_avg_wait_ms: 0.0,
            normal_priority_avg_wait_ms: 0.0,
            low_priority_avg_wait_ms: 0.0,
            high_priority_avg_queue: 0.0,
            normal_priority_avg_queue: 0.0,
            low_priority_avg_queue: 0.0,
            high_priority_loss_prob: 0.0,
            normal_priority_loss_prob: 0.0,
            low_priority_loss_prob: 0.0,
            high_priority_throughput: 0.0,
            normal_priority_throughput: 0.0,
            low_priority_throughput: 0.0,
            gps_utilizations: [0.0; 5],
            total_utilization: 0.0,
            adaptation_count: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AdaptationDecision {
    pub timestamp: Instant,
    pub from_high: f64,
    pub from_normal: f64,
    pub from_low: f64,
    pub to_high: f64,
    pub to_normal: f64,
    pub to_low: f64,
    pub reason: String,
    pub confidence: f64,
    pub predicted_improvement: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum QosError {
    #[error("Timeout waiting for QoS permit")]
    Timeout,

    #[error("Semaphore closed")]
    SemaphoreClosed,

    #[error("Insufficient data for adaptation")]
    InsufficientData,

    #[error("No adaptation needed")]
    NoAdaptationNeeded,

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("Leaky bucket full")]
    LeakyBucketFull,
}

pub struct QosPermit<'a> {
    _priority: Priority,
    _manager: &'a QosManager,
    _permit: Option<SemaphorePermit<'a>>,
    _acquired_at: Instant,
    _token_cost: f64,
}

pub struct QosManager {
    quotas: RwLock<QosQuotas>,
    pub gps_model: RwLock<GPSModel>,
    _wfq_model: RwLock<WFQModel>,
    high_priority_semaphore: Semaphore,
    normal_priority_semaphore: Semaphore,
    low_priority_semaphore: Semaphore,
    metrics: Arc<DashMap<String, f64>>,
    statistics: RwLock<QosStatistics>,
    adaptation_history: RwLock<Vec<AdaptationDecision>>,
    arrival_rate_history: RwLock<Vec<[f64; 5]>>,
    wait_time_history: RwLock<Vec<[f64; 5]>>,
    adaptation_interval: Duration,
    min_samples_for_adaptation: usize,
    adaptation_sensitivity: f64,
}

impl QosManager {
    pub fn new(
        high_priority_quota: f64,
        normal_priority_quota: f64,
        low_priority_quota: f64,
        total_capacity: usize,
    ) -> Self {
        info!("🚦 Initializing Mathematical QoS Manager v2.0");

        let quotas = QosQuotas::new(
            high_priority_quota,
            normal_priority_quota,
            low_priority_quota,
            total_capacity,
        );

        let high_capacity = (total_capacity as f64 * quotas.current_high_priority).ceil() as usize;
        let normal_capacity = (total_capacity as f64 * quotas.current_normal_priority).ceil() as usize;
        let low_capacity = (total_capacity as f64 * quotas.current_low_priority).ceil() as usize;

        info!("  High: {} permits ({:.1}%)", high_capacity, quotas.current_high_priority * 100.0);
        info!("  Normal: {} permits ({:.1}%)", normal_capacity, quotas.current_normal_priority * 100.0);
        info!("  Low: {} permits ({:.1}%)", low_capacity, quotas.current_low_priority * 100.0);
        info!("  Total capacity: {}", total_capacity);

        let metrics = Arc::new(DashMap::new());
        metrics.insert("qos.initialized".to_string(), 1.0);
        metrics.insert("qos.high_capacity".to_string(), high_capacity as f64);
        metrics.insert("qos.normal_capacity".to_string(), normal_capacity as f64);
        metrics.insert("qos.low_capacity".to_string(), low_capacity as f64);

        let mut gps_model = GPSModel::new(total_capacity as f64);
        gps_model.weights = [4.0, 2.0, 1.0, 0.5, 0.25];
        gps_model.compute_shares();

        let _wfq_model = RwLock::new(WFQModel::new());

        Self {
            quotas: RwLock::new(quotas),
            gps_model: RwLock::new(gps_model),
            _wfq_model,
            high_priority_semaphore: Semaphore::new(high_capacity),
            normal_priority_semaphore: Semaphore::new(normal_capacity),
            low_priority_semaphore: Semaphore::new(low_capacity),
            metrics,
            statistics: RwLock::new(QosStatistics::default()),
            adaptation_history: RwLock::new(Vec::with_capacity(100)),
            arrival_rate_history: RwLock::new(Vec::with_capacity(1000)),
            wait_time_history: RwLock::new(Vec::with_capacity(1000)),
            adaptation_interval: Duration::from_secs(30),
            min_samples_for_adaptation: 100,
            adaptation_sensitivity: 0.1,
        }
    }

    pub async fn acquire_permit(&self, priority: Priority) -> Result<QosPermit<'_>, QosError> {
        let start_wait = Instant::now();

        self.update_statistics(priority, false).await;

        let mut quotas = self.quotas.write().await;
        let token_cost = match priority {
            Priority::Critical | Priority::High => 1.0,
            Priority::Normal => 0.5,
            Priority::Low | Priority::Background => 0.25,
        };

        // ИСПРАВЛЕНО: Critical priority bypasses rate limiting!
        if priority == Priority::Critical {
            // Skip token bucket and leaky bucket for critical packets
            return self.acquire_permit_no_limit(priority, token_cost, start_wait).await;
        }

        let token_bucket = match priority {
            Priority::Critical | Priority::High => &mut quotas.high_token_bucket,
            Priority::Normal => &mut quotas.normal_token_bucket,
            Priority::Low | Priority::Background => &mut quotas.low_token_bucket,
        };

        if !token_bucket.try_consume(token_cost) {
            self.update_statistics(priority, true).await;
            self.record_metric(&format!("qos.rate_limit.{}", priority_to_str(priority)), 1.0);
            return Err(QosError::RateLimitExceeded);
        }

        // ИСПРАВЛЕНО: Skip leaky bucket for High priority too
        if priority == Priority::High {
            // High priority bypasses leaky bucket
            return self.acquire_permit_with_semaphore(priority, token_cost, start_wait).await;
        }

        let leaky_bucket = match priority {
            Priority::Critical | Priority::High => &mut quotas.high_leaky_bucket,
            Priority::Normal => &mut quotas.normal_leaky_bucket,
            Priority::Low | Priority::Background => &mut quotas.low_leaky_bucket,
        };

        if !leaky_bucket.try_add(1.0) {
            self.update_statistics(priority, true).await;
            self.record_metric(&format!("qos.leaky_bucket_full.{}", priority_to_str(priority)), 1.0);
            return Err(QosError::LeakyBucketFull);
        }

        drop(quotas);
        self.acquire_permit_with_semaphore(priority, token_cost, start_wait).await
    }

    async fn acquire_permit_with_semaphore(
        &self,
        priority: Priority,
        _token_cost: f64,
        start_wait: Instant
    ) -> Result<QosPermit<'_>, QosError> {
        let permit_result = match priority {
            Priority::Critical | Priority::High => {
                tokio::time::timeout(
                    Duration::from_millis(50),
                    self.high_priority_semaphore.acquire()
                ).await
            }
            Priority::Normal => {
                tokio::time::timeout(
                    Duration::from_millis(100),
                    self.normal_priority_semaphore.acquire()
                ).await
            }
            Priority::Low | Priority::Background => {
                tokio::time::timeout(
                    Duration::from_millis(200),
                    self.low_priority_semaphore.acquire()
                ).await
            }
        };

        match permit_result {
            Ok(Ok(permit_owned)) => {
                let wait_time = start_wait.elapsed();
                self.record_wait_time(priority, wait_time).await;

                self.record_metric(
                    &format!("qos.acquire_success.{}", priority_to_str(priority)),
                    1.0
                );
                self.record_metric(
                    &format!("qos.{}_wait_ms", priority_to_str(priority)),
                    wait_time.as_millis() as f64
                );

                Ok(QosPermit {
                    _priority: priority,
                    _manager: self,
                    _permit: Some(permit_owned),
                    _acquired_at: Instant::now(),
                    _token_cost,
                })
            }
            Ok(Err(_)) => {
                self.update_statistics(priority, true).await;
                self.record_metric(
                    &format!("qos.acquire_failed.{}", priority_to_str(priority)),
                    1.0
                );
                Err(QosError::SemaphoreClosed)
            }
            Err(_) => {
                self.update_statistics(priority, true).await;
                self.record_metric(
                    &format!("qos.acquire_timeout.{}", priority_to_str(priority)),
                    1.0
                );
                Err(QosError::Timeout)
            }
        }
    }

    async fn acquire_permit_no_limit(&self, priority: Priority, token_cost: f64, start_wait: Instant) -> Result<QosPermit<'_>, QosError> {
        // Use the same semaphore logic but without rate limiting
        self.acquire_permit_with_semaphore(priority, token_cost, start_wait).await
    }

    pub async fn adapt_quotas(&self) -> Result<AdaptationDecision, QosError> {
        let quotas = self.quotas.read().await;
        let stats = self.statistics.read().await;

        // Проверка достаточности данных
        let total_requests = stats.high_priority_requests +
            stats.normal_priority_requests +
            stats.low_priority_requests;

        if total_requests < self.min_samples_for_adaptation as u64 {
            return Err(QosError::InsufficientData);
        }

        // Расчёт вероятностей отказов
        let high_rejection = if stats.high_priority_requests > 0 {
            stats.high_priority_rejected as f64 / stats.high_priority_requests as f64
        } else { 0.0 };

        let normal_rejection = if stats.normal_priority_requests > 0 {
            stats.normal_priority_rejected as f64 / stats.normal_priority_requests as f64
        } else { 0.0 };

        let low_rejection = if stats.low_priority_requests > 0 {
            stats.low_priority_rejected as f64 / stats.low_priority_requests as f64
        } else { 0.0 };

        // Расчёт среднего времени ожидания
        let high_wait = stats.high_priority_avg_wait_ms;
        let normal_wait = stats.normal_priority_avg_wait_ms;
        let low_wait = stats.low_priority_avg_wait_ms;

        let mut new_high = quotas.current_high_priority;
        let mut new_normal = quotas.current_normal_priority;
        let mut new_low = quotas.current_low_priority;
        let mut reason = String::new();
        let mut confidence = 0.7;
        let mut predicted_improvement = 0.0;

        // Целевая функция: минимизация взвешенной суммы отказов и задержек
        let alpha = 0.6; // вес отказов
        let beta = 0.4;  // вес задержек

        let _current_cost = alpha * (
            high_rejection * 4.0 +
                normal_rejection * 2.0 +
                low_rejection * 1.0
        ) + beta * (
            high_wait / 50.0 * 4.0 +
                normal_wait / 100.0 * 2.0 +
                low_wait / 200.0 * 1.0
        );

        // === Адаптация для High приоритета ===
        if high_rejection > 0.05 {
            // Слишком много отказов - увеличиваем квоту
            let increase = (high_rejection * self.adaptation_sensitivity * 2.0).min(0.1);

            if new_low > 0.1 {
                new_low = (new_low - increase).max(0.1);
                new_high = (new_high + increase).min(0.7);
                reason = format!("High priority rejection {:.1}% > 5%, taking {:.1}% from low",
                                 high_rejection * 100.0, increase * 100.0);
                confidence = 0.8;
                predicted_improvement = -increase * 10.0;
            } else if new_normal > 0.3 {
                new_normal = (new_normal - increase * 0.5).max(0.2);
                new_high = (new_high + increase * 0.5).min(0.7);
                reason = format!("High priority rejection {:.1}% > 5%, taking from normal",
                                 high_rejection * 100.0);
                confidence = 0.7;
                predicted_improvement = -increase * 5.0;
            }
        }

        // === Адаптация для Normal приоритета ===
        else if normal_rejection > 0.1 {
            let increase = (normal_rejection * self.adaptation_sensitivity).min(0.05);

            if new_low > 0.15 {
                new_low = (new_low - increase).max(0.1);
                new_normal = (new_normal + increase).min(0.5);
                reason = format!("Normal priority rejection {:.1}% > 10%, taking from low",
                                 normal_rejection * 100.0);
                confidence = 0.75;
                predicted_improvement = -increase * 8.0;
            }
        }

        // === Адаптация для Low приоритета (защита от голодания) ===
        else if low_rejection > 0.2 && quotas.current_low_priority < quotas.base_low_priority * 1.5 {
            let increase = 0.02;

            if new_high > 0.3 {
                new_high = (new_high - increase).max(0.2);
                new_low = (new_low + increase).min(0.3);
                reason = format!("Low priority starvation, increasing quota by {:.1}%", increase * 100.0);
                confidence = 0.85;
                predicted_improvement = -5.0;
            }
        }

        // === Адаптация на основе времени ожидания ===
        else if high_wait > 100.0 && quotas.current_high_priority > 0.3 {
            // High priority latency too high - reduce quota
            let decrease = 0.03;
            new_high = (new_high - decrease).max(0.2);
            new_normal = (new_normal + decrease * 0.5).min(0.5);
            new_low = (new_low + decrease * 0.5).min(0.3);
            reason = format!("High priority latency {:.1}ms > 100ms, reducing quota", high_wait);
            confidence = 0.7;
            predicted_improvement = 10.0;
        }

        else if normal_wait > 200.0 && quotas.current_normal_priority > 0.3 {
            let decrease = 0.02;
            new_normal = (new_normal - decrease).max(0.2);
            new_low = (new_low + decrease).min(0.3);
            reason = format!("Normal priority latency {:.1}ms > 200ms, reducing quota", normal_wait);
            confidence = 0.7;
            predicted_improvement = 8.0;
        }

        if reason.is_empty() {
            return Err(QosError::NoAdaptationNeeded);
        }

        // Нормировка квот
        let total = new_high + new_normal + new_low;
        new_high /= total;
        new_normal /= total;
        new_low /= total;

        let decision = AdaptationDecision {
            timestamp: Instant::now(),
            from_high: quotas.current_high_priority,
            from_normal: quotas.current_normal_priority,
            from_low: quotas.current_low_priority,
            to_high: new_high,
            to_normal: new_normal,
            to_low: new_low,
            reason,
            confidence,
            predicted_improvement,
        };

        drop(quotas);
        self.apply_adaptation(&decision).await?;

        // Сохранение в историю
        let mut history = self.adaptation_history.write().await;
        history.push(decision.clone());
        if history.len() > 100 {
            history.remove(0);
        }

        Ok(decision)
    }

    async fn apply_adaptation(&self, decision: &AdaptationDecision) -> Result<(), QosError> {
        let mut quotas = self.quotas.write().await;

        // Расчёт новых ёмкостей
        let high_capacity = (quotas.total_capacity as f64 * decision.to_high).ceil() as usize;
        let normal_capacity = (quotas.total_capacity as f64 * decision.to_normal).ceil() as usize;
        let low_capacity = (quotas.total_capacity as f64 * decision.to_low).ceil() as usize;

        // Получение текущих доступных пермитов
        let high_available = self.high_priority_semaphore.available_permits();
        let normal_available = self.normal_priority_semaphore.available_permits();
        let low_available = self.low_priority_semaphore.available_permits();

        // Расчёт изменений
        let high_change = high_capacity as isize - high_available as isize;
        let normal_change = normal_capacity as isize - normal_available as isize;
        let low_change = low_capacity as isize - low_available as isize;

        // Применение изменений
        if high_change > 0 {
            self.high_priority_semaphore.add_permits(high_change as usize);
        }
        if normal_change > 0 {
            self.normal_priority_semaphore.add_permits(normal_change as usize);
        }
        if low_change > 0 {
            self.low_priority_semaphore.add_permits(low_change as usize);
        }

        // Обновление квот
        quotas.current_high_priority = decision.to_high;
        quotas.current_normal_priority = decision.to_normal;
        quotas.current_low_priority = decision.to_low;
        quotas.last_adaptation = Instant::now();

        // Обновление GPS модели
        quotas.gps_model.total_capacity = quotas.total_capacity as f64;
        quotas.gps_model.weights = [4.0, 2.0, 1.0, 0.5, 0.25];
        quotas.gps_model.compute_shares();

        // Обновление token buckets
        quotas.high_token_bucket = TokenBucket::new(
            high_capacity as f64 / 1000.0,
            high_capacity as f64
        );
        quotas.normal_token_bucket = TokenBucket::new(
            normal_capacity as f64 / 1000.0,
            normal_capacity as f64
        );
        quotas.low_token_bucket = TokenBucket::new(
            low_capacity as f64 / 1000.0,
            low_capacity as f64
        );

        // Обновление leaky buckets
        quotas.high_leaky_bucket = LeakyBucket::new(
            high_capacity as f64 / 1000.0,
            high_capacity as f64
        );
        quotas.normal_leaky_bucket = LeakyBucket::new(
            normal_capacity as f64 / 1000.0,
            normal_capacity as f64
        );
        quotas.low_leaky_bucket = LeakyBucket::new(
            low_capacity as f64 / 1000.0,
            low_capacity as f64
        );

        // Запись метрик
        self.record_metric("qos.adaptation", 1.0);
        self.record_metric("qos.high_quota", decision.to_high);
        self.record_metric("qos.normal_quota", decision.to_normal);
        self.record_metric("qos.low_quota", decision.to_low);
        self.record_metric("qos.high_capacity", high_capacity as f64);
        self.record_metric("qos.normal_capacity", normal_capacity as f64);
        self.record_metric("qos.low_capacity", low_capacity as f64);
        self.record_metric("qos.adaptation_confidence", decision.confidence);

        // Обновление статистики
        let mut stats = self.statistics.write().await;
        stats.adaptation_count += 1;

        info!("🔄 QoS adaptation applied:");
        info!("  High: {:.1}% → {:.1}%", decision.from_high * 100.0, decision.to_high * 100.0);
        info!("  Normal: {:.1}% → {:.1}%", decision.from_normal * 100.0, decision.to_normal * 100.0);
        info!("  Low: {:.1}% → {:.1}%", decision.from_low * 100.0, decision.to_low * 100.0);
        info!("  Reason: {}", decision.reason);
        info!("  Confidence: {:.1}%", decision.confidence * 100.0);
        info!("  Predicted improvement: {:.1}%", decision.predicted_improvement);

        Ok(())
    }

    async fn update_statistics(&self, priority: Priority, rejected: bool) {
        let mut stats = self.statistics.write().await;

        match priority {
            Priority::Critical | Priority::High => {
                stats.high_priority_requests += 1;
                if rejected {
                    stats.high_priority_rejected += 1;
                    stats.high_priority_loss_prob = stats.high_priority_rejected as f64 /
                        stats.high_priority_requests as f64;
                }
            }
            Priority::Normal => {
                stats.normal_priority_requests += 1;
                if rejected {
                    stats.normal_priority_rejected += 1;
                    stats.normal_priority_loss_prob = stats.normal_priority_rejected as f64 /
                        stats.normal_priority_requests as f64;
                }
            }
            Priority::Low | Priority::Background => {
                stats.low_priority_requests += 1;
                if rejected {
                    stats.low_priority_rejected += 1;
                    stats.low_priority_loss_prob = stats.low_priority_rejected as f64 /
                        stats.low_priority_requests as f64;
                }
            }
        }
    }

    async fn record_wait_time(&self, priority: Priority, wait_time: Duration) {
        let mut stats = self.statistics.write().await;
        let alpha = 0.1; // коэффициент EMA
        let wait_ms = wait_time.as_millis() as f64;

        match priority {
            Priority::Critical | Priority::High => {
                stats.high_priority_avg_wait_ms =
                    stats.high_priority_avg_wait_ms * (1.0 - alpha) + wait_ms * alpha;

                // Обновление средней длины очереди через Little's Law
                stats.high_priority_avg_queue =
                    stats.high_priority_avg_wait_ms * stats.high_priority_requests as f64 / 1000.0;

                self.record_metric("qos.high_avg_wait_ms", stats.high_priority_avg_wait_ms);
                self.record_metric("qos.high_avg_queue", stats.high_priority_avg_queue);
            }
            Priority::Normal => {
                stats.normal_priority_avg_wait_ms =
                    stats.normal_priority_avg_wait_ms * (1.0 - alpha) + wait_ms * alpha;

                stats.normal_priority_avg_queue =
                    stats.normal_priority_avg_wait_ms * stats.normal_priority_requests as f64 / 1000.0;

                self.record_metric("qos.normal_avg_wait_ms", stats.normal_priority_avg_wait_ms);
                self.record_metric("qos.normal_avg_queue", stats.normal_priority_avg_queue);
            }
            Priority::Low | Priority::Background => {
                stats.low_priority_avg_wait_ms =
                    stats.low_priority_avg_wait_ms * (1.0 - alpha) + wait_ms * alpha;

                stats.low_priority_avg_queue =
                    stats.low_priority_avg_wait_ms * stats.low_priority_requests as f64 / 1000.0;

                self.record_metric("qos.low_avg_wait_ms", stats.low_priority_avg_wait_ms);
                self.record_metric("qos.low_avg_queue", stats.low_priority_avg_queue);
            }
        }

        // Сохранение в историю
        let mut history = self.wait_time_history.write().await;
        let mut times = [0.0; 5];
        times[priority_to_class(priority)] = wait_ms;
        history.push(times);
        if history.len() > 1000 {
            history.remove(0);
        }
    }

    pub async fn update_models(&self, arrival_rates: [f64; 5], batch_size: f64) {
        let mut quotas = self.quotas.write().await;
        let mut gps = self.gps_model.write().await;

        // Обновление GPS модели
        gps.arrival_rates = arrival_rates;
        gps.compute_utilization(batch_size);

        // Обновление квот
        quotas.update_models(arrival_rates, batch_size);

        // Обновление статистики
        let mut stats = self.statistics.write().await;
        stats.gps_utilizations = gps.utilizations;
        stats.total_utilization = gps.total_utilization;

        // Расчёт throughput для каждого класса
        for i in 0..5 {
            match i {
                0 | 1 => stats.high_priority_throughput = gps.throughput(i),
                2 => stats.normal_priority_throughput = gps.throughput(i),
                3 | 4 => stats.low_priority_throughput = gps.throughput(i),
                _ => {}
            }
        }

        self.record_metric("qos.gps_total_utilization", gps.total_utilization);
        self.record_metric("qos.gps_high_utilization", gps.utilizations[0]);
        self.record_metric("qos.gps_normal_utilization", gps.utilizations[2]);
        self.record_metric("qos.gps_low_utilization", gps.utilizations[3]);

        // Сохранение в историю
        let mut history = self.arrival_rate_history.write().await;
        history.push(arrival_rates);
        if history.len() > 1000 {
            history.remove(0);
        }
    }

    fn record_metric(&self, key: &str, value: f64) {
        self.metrics.insert(key.to_string(), value);
    }

    pub async fn get_statistics(&self) -> QosStatistics {
        self.statistics.read().await.clone()
    }

    pub async fn get_quotas(&self) -> (f64, f64, f64) {
        let quotas = self.quotas.read().await;
        (quotas.current_high_priority, quotas.current_normal_priority, quotas.current_low_priority)
    }

    pub async fn get_utilization(&self) -> (f64, f64, f64) {
        let high_available = self.high_priority_semaphore.available_permits();
        let normal_available = self.normal_priority_semaphore.available_permits();
        let low_available = self.low_priority_semaphore.available_permits();

        let quotas = self.quotas.read().await;
        let high_capacity = (quotas.total_capacity as f64 * quotas.current_high_priority).ceil() as usize;
        let normal_capacity = (quotas.total_capacity as f64 * quotas.current_normal_priority).ceil() as usize;
        let low_capacity = (quotas.total_capacity as f64 * quotas.current_low_priority).ceil() as usize;

        let high_util = if high_capacity > 0 {
            1.0 - (high_available as f64 / high_capacity as f64)
        } else { 0.0 };

        let normal_util = if normal_capacity > 0 {
            1.0 - (normal_available as f64 / normal_capacity as f64)
        } else { 0.0 };

        let low_util = if low_capacity > 0 {
            1.0 - (low_available as f64 / low_capacity as f64)
        } else { 0.0 };

        (high_util, normal_util, low_util)
    }
}

fn priority_to_str(priority: Priority) -> &'static str {
    match priority {
        Priority::Critical | Priority::High => "high",
        Priority::Normal => "normal",
        Priority::Low | Priority::Background => "low",
    }
}

fn priority_to_class(priority: Priority) -> usize {
    match priority {
        Priority::Critical => 0,
        Priority::High => 1,
        Priority::Normal => 2,
        Priority::Low => 3,
        Priority::Background => 4,
    }
}

impl Clone for QosManager {
    fn clone(&self) -> Self {
        let quotas = self.quotas.try_read()
            .map(|q| q.clone())
            .unwrap_or_else(|_| {
                QosQuotas::new(0.4, 0.4, 0.2, 100000)
            });

        let high_capacity = (quotas.total_capacity as f64 * quotas.current_high_priority).ceil() as usize;
        let normal_capacity = (quotas.total_capacity as f64 * quotas.current_normal_priority).ceil() as usize;
        let low_capacity = (quotas.total_capacity as f64 * quotas.current_low_priority).ceil() as usize;

        let metrics = Arc::new(DashMap::new());
        for entry in self.metrics.iter() {
            metrics.insert(entry.key().clone(), *entry.value());
        }

        let gps_model = self.gps_model.try_read()
            .map(|g| g.clone())
            .unwrap_or_else(|_| GPSModel::new(100000.0));

        Self {
            quotas: RwLock::new(quotas),
            gps_model: RwLock::new(gps_model),
            _wfq_model: RwLock::new(WFQModel::new()),
            high_priority_semaphore: Semaphore::new(high_capacity),
            normal_priority_semaphore: Semaphore::new(normal_capacity),
            low_priority_semaphore: Semaphore::new(low_capacity),
            metrics,
            statistics: RwLock::new(QosStatistics::default()),
            adaptation_history: RwLock::new(Vec::new()),
            arrival_rate_history: RwLock::new(Vec::new()),
            wait_time_history: RwLock::new(Vec::new()),
            adaptation_interval: self.adaptation_interval,
            min_samples_for_adaptation: self.min_samples_for_adaptation,
            adaptation_sensitivity: self.adaptation_sensitivity,
        }
    }
}