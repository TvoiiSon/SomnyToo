use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore, Mutex};
use tracing::{info, warn, debug};
use dashmap::DashMap;

use crate::core::protocol::batch_system::types::priority::Priority;

/// QoS Manager для приоритизации трафика с динамическим перераспределением
pub struct QosManager {
    quotas: RwLock<QosQuotas>,
    semaphores: QosSemaphores,
    metrics: Arc<DashMap<String, f64>>,
    statistics: RwLock<QosStatistics>,
    dynamic_policy: Mutex<DynamicPolicy>,
    adaptation_engine: Arc<AdaptationEngine>,
}

#[derive(Debug, Clone)]
pub struct QosQuotas {
    pub base_high_priority: f64,
    pub base_normal_priority: f64,
    pub base_low_priority: f64,
    pub current_high_priority: f64,
    pub current_normal_priority: f64,
    pub current_low_priority: f64,
    pub total_capacity: usize,
    pub last_adaptation: Instant,
}

#[derive(Debug)]
struct QosSemaphores {
    high_priority: Semaphore,
    normal_priority: Semaphore,
    low_priority: Semaphore,
}

#[derive(Debug, Clone)]
pub struct QosStatistics {
    pub high_priority_requests: u64,
    pub normal_priority_requests: u64,
    pub low_priority_requests: u64,
    pub high_priority_rejected: u64,
    pub normal_priority_rejected: u64,
    pub low_priority_rejected: u64,
    pub high_priority_wait_time: Duration,
    pub normal_priority_wait_time: Duration,
    pub low_priority_wait_time: Duration,
    pub high_priority_avg_wait_ms: f64,
    pub normal_priority_avg_wait_ms: f64,
    pub low_priority_avg_wait_ms: f64,
}

#[derive(Debug, Clone)]
pub struct DynamicPolicy {
    pub enabled: bool,
    pub adaptation_interval: Duration,
    pub target_high_priority_latency_ms: f64,
    pub target_normal_priority_latency_ms: f64,
    pub max_quota_change_per_step: f64,
    pub min_quota_per_class: f64,
    pub starvation_prevention_enabled: bool,
    pub starvation_threshold: u64,
}

impl Default for DynamicPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            adaptation_interval: Duration::from_secs(10),
            target_high_priority_latency_ms: 10.0,
            target_normal_priority_latency_ms: 50.0,
            max_quota_change_per_step: 0.2,
            min_quota_per_class: 0.05,
            starvation_prevention_enabled: true,
            starvation_threshold: 1000,
        }
    }
}

struct AdaptationEngine {
    load_history: RwLock<Vec<LoadSample>>,
    decision_history: RwLock<Vec<AdaptationDecision>>,
    max_history_size: usize,
}

#[derive(Debug, Clone)]
struct LoadSample {
    timestamp: Instant,
    high_load: f64,
    normal_load: f64,
    low_load: f64,
    high_rejection_rate: f64,
    normal_rejection_rate: f64,
    low_rejection_rate: f64,
    system_load: f64,
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
    pub improvement_expected: f64,
}

// Публичная структура для анализа нагрузки
#[derive(Debug, Clone)]
pub struct LoadAnalysis {
    pub timestamp: Instant,
    pub high_rejection_rate: f64,
    pub normal_rejection_rate: f64,
    pub low_rejection_rate: f64,
    pub high_utilization: f64,
    pub normal_utilization: f64,
    pub low_utilization: f64,
    pub high_latency_violation: bool,
    pub normal_latency_violation: bool,
    pub low_starvation: bool,
    pub total_requests: u64,
    pub system_load: f64,
}

impl QosManager {
    pub fn new(
        high_priority_quota: f64,
        normal_priority_quota: f64,
        low_priority_quota: f64,
        total_capacity: usize,
    ) -> Self {
        // Проверка квот
        let total = high_priority_quota + normal_priority_quota + low_priority_quota;
        if (total - 1.0).abs() > 0.01 {
            warn!("QoS quotas don't sum to 1.0 ({}), normalizing", total);
        }

        let normalized_high = high_priority_quota / total;
        let normalized_normal = normal_priority_quota / total;
        let normalized_low = low_priority_quota / total;

        let quotas = QosQuotas {
            base_high_priority: normalized_high,
            base_normal_priority: normalized_normal,
            base_low_priority: normalized_low,
            current_high_priority: normalized_high,
            current_normal_priority: normalized_normal,
            current_low_priority: normalized_low,
            total_capacity,
            last_adaptation: Instant::now(),
        };

        let high_capacity = (total_capacity as f64 * normalized_high).ceil() as usize;
        let normal_capacity = (total_capacity as f64 * normalized_normal).ceil() as usize;
        let low_capacity = (total_capacity as f64 * normalized_low).ceil() as usize;

        info!("🚦 QoS Manager initialized with dynamic adaptation:");
        info!("  - Base High: {} permits ({}%)", high_capacity, normalized_high * 100.0);
        info!("  - Base Normal: {} permits ({}%)", normal_capacity, normalized_normal * 100.0);
        info!("  - Base Low: {} permits ({}%)", low_capacity, normalized_low * 100.0);
        info!("  - Total capacity: {}", total_capacity);

        let policy = DynamicPolicy::default();

        let adaptation_engine = Arc::new(AdaptationEngine {
            load_history: RwLock::new(Vec::new()),
            decision_history: RwLock::new(Vec::new()),
            max_history_size: 100,
        });

        let manager = Self {
            quotas: RwLock::new(quotas),
            semaphores: QosSemaphores {
                high_priority: Semaphore::new(high_capacity),
                normal_priority: Semaphore::new(normal_capacity),
                low_priority: Semaphore::new(low_capacity),
            },
            metrics: Arc::new(DashMap::new()),
            statistics: RwLock::new(QosStatistics {
                high_priority_requests: 0,
                normal_priority_requests: 0,
                low_priority_requests: 0,
                high_priority_rejected: 0,
                normal_priority_rejected: 0,
                low_priority_rejected: 0,
                high_priority_wait_time: Duration::from_secs(0),
                normal_priority_wait_time: Duration::from_secs(0),
                low_priority_wait_time: Duration::from_secs(0),
                high_priority_avg_wait_ms: 0.0,
                normal_priority_avg_wait_ms: 0.0,
                low_priority_avg_wait_ms: 0.0,
            }),
            dynamic_policy: Mutex::new(policy),
            adaptation_engine: adaptation_engine.clone(),
        };

        // Запускаем фоновую задачу адаптации
        manager.start_adaptation_background_task();

        manager
    }

    /// Получить разрешение на обработку по приоритету
    pub async fn acquire_permit(&self, priority: Priority) -> Result<QosPermit<'_>, QosError> {
        let start_time = Instant::now();

        let semaphore = match priority {
            Priority::Critical | Priority::High => &self.semaphores.high_priority,
            Priority::Normal => &self.semaphores.normal_priority,
            Priority::Low | Priority::Background => &self.semaphores.low_priority,
        };

        // Обновляем статистику запросов
        self.update_statistics(priority, false).await;

        // Пытаемся получить разрешение с таймаутом, зависящим от приоритета
        let timeout = match priority {
            Priority::Critical => Duration::from_millis(10),
            Priority::High => Duration::from_millis(20),
            Priority::Normal => Duration::from_millis(100),
            Priority::Low | Priority::Background => Duration::from_millis(200),
        };

        let permit = match tokio::time::timeout(timeout, semaphore.acquire()).await {
            Ok(Ok(permit)) => {
                let wait_time = start_time.elapsed();
                self.record_wait_time(priority, wait_time).await;
                permit
            }
            Ok(Err(_)) => {
                self.update_statistics(priority, true).await;
                return Err(QosError::SemaphoreClosed);
            }
            Err(_) => {
                self.update_statistics(priority, true).await;
                return Err(QosError::Timeout);
            }
        };

        // Обновляем метрики нагрузки
        self.update_load_metrics().await;

        Ok(QosPermit {
            priority,
            _permit: permit,
            manager: self,
        })
    }

    /// Внутренний метод для освобождения разрешения (вызывается из QosPermit)
    fn release_permit(&self, priority: Priority) {
        match priority {
            Priority::Critical | Priority::High => self.semaphores.high_priority.add_permits(1),
            Priority::Normal => self.semaphores.normal_priority.add_permits(1),
            Priority::Low | Priority::Background => self.semaphores.low_priority.add_permits(1),
        }
    }

    /// Динамическое обновление квот на основе анализа нагрузки
    pub async fn adapt_quotas_based_on_load(&self) -> Result<AdaptationDecision, QosError> {
        let policy = self.dynamic_policy.lock().await;

        if !policy.enabled {
            return Err(QosError::AdaptationDisabled);
        }

        let quotas = self.quotas.read().await;
        let now = Instant::now();

        // Проверяем, прошло ли достаточно времени с последней адаптации
        if now.duration_since(quotas.last_adaptation) < policy.adaptation_interval {
            return Err(QosError::TooFrequentAdaptation);
        }

        // Анализируем текущую нагрузку
        let load_analysis = self.analyze_current_load().await;

        // Записываем образец нагрузки в историю
        self.adaptation_engine.record_load_sample(&load_analysis).await;

        // Принимаем решение об адаптации
        let decision = self.make_adaptation_decision(&policy, &quotas, &load_analysis).await;

        // Применяем решение
        self.apply_adaptation_decision(decision.clone()).await?;

        Ok(decision)
    }

    async fn analyze_current_load(&self) -> LoadAnalysis {
        let stats = self.statistics.read().await;

        let total_requests = stats.high_priority_requests +
            stats.normal_priority_requests +
            stats.low_priority_requests;

        let high_rejection_rate = if stats.high_priority_requests > 0 {
            stats.high_priority_rejected as f64 / stats.high_priority_requests as f64
        } else { 0.0 };

        let normal_rejection_rate = if stats.normal_priority_requests > 0 {
            stats.normal_priority_rejected as f64 / stats.normal_priority_requests as f64
        } else { 0.0 };

        let low_rejection_rate = if stats.low_priority_requests > 0 {
            stats.low_priority_rejected as f64 / stats.low_priority_requests as f64
        } else { 0.0 };

        // Получаем текущую утилизацию
        let (high_util, normal_util, low_util) = self.get_utilization().await;

        // Анализируем латенси
        let high_latency_violation = stats.high_priority_avg_wait_ms > 20.0;
        let normal_latency_violation = stats.normal_priority_avg_wait_ms > 100.0;

        // Проверяем голодание низкоприоритетных задач
        let low_starvation = stats.low_priority_requests > 100 &&
            stats.low_priority_rejected as f64 / stats.low_priority_requests as f64 > 0.5;

        // Рассчитываем общую нагрузку системы
        let system_load = (high_util + normal_util + low_util) / 3.0;

        LoadAnalysis {
            timestamp: Instant::now(),
            high_rejection_rate,
            normal_rejection_rate,
            low_rejection_rate,
            high_utilization: high_util,
            normal_utilization: normal_util,
            low_utilization: low_util,
            high_latency_violation,
            normal_latency_violation,
            low_starvation,
            total_requests,
            system_load,
        }
    }

    async fn make_adaptation_decision(
        &self,
        policy: &DynamicPolicy,
        quotas: &QosQuotas,
        analysis: &LoadAnalysis,
    ) -> AdaptationDecision {
        let mut new_high = quotas.current_high_priority;
        let mut new_normal = quotas.current_normal_priority;
        let mut new_low = quotas.current_low_priority;

        let mut reason = String::new();
        let mut improvement_expected = 0.0;

        // Приоритет 1: Высокоприоритетные задачи превышают latency SLA
        if analysis.high_latency_violation && analysis.high_rejection_rate > 0.1 {
            let increase = (policy.max_quota_change_per_step * 0.5_f64).min(0.15_f64);
            if new_low > policy.min_quota_per_class + increase {
                new_low -= increase;
                new_high += increase;
                reason = format!("High priority latency violation ({}ms avg, {:.1}% rejection), taking from low",
                                 analysis.high_rejection_rate * 1000.0, analysis.high_rejection_rate * 100.0);
                improvement_expected = 0.3;
            }
        }
        // Приоритет 2: Нормальные задачи превышают latency SLA
        else if analysis.normal_latency_violation && analysis.normal_rejection_rate > 0.2 {
            let increase = (policy.max_quota_change_per_step * 0.3_f64).min(0.1_f64);
            if new_low > policy.min_quota_per_class + increase {
                new_low -= increase;
                new_normal += increase;
                reason = format!("Normal priority latency violation ({}ms avg, {:.1}% rejection), taking from low",
                                 analysis.normal_rejection_rate * 1000.0, analysis.normal_rejection_rate * 100.0);
                improvement_expected = 0.2;
            }
        }
        // Приоритет 3: Предотвращение голодания низкоприоритетных задач
        else if policy.starvation_prevention_enabled && analysis.low_starvation {
            let increase = (policy.max_quota_change_per_step * 0.2_f64).min(0.05_f64);
            if new_high > policy.min_quota_per_class + increase {
                new_high -= increase;
                new_low += increase;
                reason = format!("Low priority starvation prevention ({:.1}% rejection)", analysis.low_rejection_rate * 100.0);
                improvement_expected = 0.1;
            }
        }
        // Приоритет 4: Возврат к базовым квотам при низкой нагрузке
        else if analysis.total_requests < 100 &&
            analysis.system_load < 0.3_f64 &&
            (new_high - quotas.base_high_priority).abs() > 0.05_f64 {
            // Плавный возврат к базовым значениям
            let step = policy.max_quota_change_per_step * 0.1_f64;
            new_high = Self::move_toward(new_high, quotas.base_high_priority, step);
            new_normal = Self::move_toward(new_normal, quotas.base_normal_priority, step);
            new_low = 1.0 - new_high - new_normal;
            reason = format!("Returning to base quotas due to low load ({:.1}% system load)", analysis.system_load * 100.0);
        }

        // Гарантируем минимальные квоты
        new_high = new_high.max(policy.min_quota_per_class);
        new_normal = new_normal.max(policy.min_quota_per_class);
        new_low = new_low.max(policy.min_quota_per_class);

        // Нормализуем
        let total = new_high + new_normal + new_low;
        if total > 0.0 {
            new_high /= total;
            new_normal /= total;
            new_low /= total;
        }

        AdaptationDecision {
            timestamp: Instant::now(),
            from_high: quotas.current_high_priority,
            from_normal: quotas.current_normal_priority,
            from_low: quotas.current_low_priority,
            to_high: new_high,
            to_normal: new_normal,
            to_low: new_low,
            reason,
            improvement_expected,
        }
    }

    fn move_toward(current: f64, target: f64, step: f64) -> f64 {
        if current < target {
            (current + step).min(target)
        } else {
            (current - step).max(target)
        }
    }

    async fn apply_adaptation_decision(&self, decision: AdaptationDecision) -> Result<(), QosError> {
        let mut quotas = self.quotas.write().await;

        // Рассчитываем новые емкости
        let high_capacity = (quotas.total_capacity as f64 * decision.to_high).ceil() as usize;
        let normal_capacity = (quotas.total_capacity as f64 * decision.to_normal).ceil() as usize;
        let low_capacity = (quotas.total_capacity as f64 * decision.to_low).ceil() as usize;

        // Получаем текущие доступные разрешения
        let high_available = self.semaphores.high_priority.available_permits();
        let normal_available = self.semaphores.normal_priority.available_permits();
        let low_available = self.semaphores.low_priority.available_permits();

        // Рассчитываем изменения
        let high_change = high_capacity as isize - high_available as isize;
        let normal_change = normal_capacity as isize - normal_available as isize;
        let low_change = low_capacity as isize - low_available as isize;

        // Применяем изменения
        if high_change > 0 {
            self.semaphores.high_priority.add_permits(high_change as usize);
        }
        if normal_change > 0 {
            self.semaphores.normal_priority.add_permits(normal_change as usize);
        }
        if low_change > 0 {
            self.semaphores.low_priority.add_permits(low_change as usize);
        }

        // Обновляем квоты
        quotas.current_high_priority = decision.to_high;
        quotas.current_normal_priority = decision.to_normal;
        quotas.current_low_priority = decision.to_low;
        quotas.last_adaptation = Instant::now();

        // Записываем решение в историю
        self.adaptation_engine.record_decision(decision.clone()).await;

        info!("🔄 QoS адаптация применена:");
        info!("  High: {:.1}% → {:.1}%", decision.from_high * 100.0, decision.to_high * 100.0);
        info!("  Normal: {:.1}% → {:.1}%", decision.from_normal * 100.0, decision.to_normal * 100.0);
        info!("  Low: {:.1}% → {:.1}%", decision.from_low * 100.0, decision.to_low * 100.0);
        info!("  Причина: {}", decision.reason);
        info!("  Ожидаемое улучшение: {:.1}%", decision.improvement_expected * 100.0);

        Ok(())
    }

    fn start_adaptation_background_task(&self) {
        let manager = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));

            loop {
                interval.tick().await;

                match manager.adapt_quotas_based_on_load().await {
                    Ok(decision) => {
                        debug!("✅ QoS адаптация выполнена: {}", decision.reason);
                    }
                    Err(QosError::TooFrequentAdaptation) => {
                        // Игнорируем, это нормально
                    }
                    Err(QosError::AdaptationDisabled) => {
                        // Адаптация отключена
                        break;
                    }
                    Err(e) => {
                        warn!("⚠️ QoS адаптация не удалась: {}", e);
                    }
                }
            }
        });
    }

    async fn update_statistics(&self, priority: Priority, rejected: bool) {
        let mut stats = self.statistics.write().await;

        match priority {
            Priority::Critical | Priority::High => {
                stats.high_priority_requests += 1;
                if rejected { stats.high_priority_rejected += 1; }
            }
            Priority::Normal => {
                stats.normal_priority_requests += 1;
                if rejected { stats.normal_priority_rejected += 1; }
            }
            Priority::Low | Priority::Background => {
                stats.low_priority_requests += 1;
                if rejected { stats.low_priority_rejected += 1; }
            }
        }

        // Обновляем метрики
        self.update_metrics(&stats).await;
    }

    async fn record_wait_time(&self, priority: Priority, wait_time: Duration) {
        let mut stats = self.statistics.write().await;

        // Экспоненциальное скользящее среднее для времени ожидания
        let alpha = 0.1_f64;
        let wait_ms = wait_time.as_millis() as f64;

        match priority {
            Priority::Critical | Priority::High => {
                stats.high_priority_wait_time += wait_time;
                stats.high_priority_avg_wait_ms =
                    stats.high_priority_avg_wait_ms * (1.0 - alpha) + wait_ms * alpha;
            }
            Priority::Normal => {
                stats.normal_priority_wait_time += wait_time;
                stats.normal_priority_avg_wait_ms =
                    stats.normal_priority_avg_wait_ms * (1.0 - alpha) + wait_ms * alpha;
            }
            Priority::Low | Priority::Background => {
                stats.low_priority_wait_time += wait_time;
                stats.low_priority_avg_wait_ms =
                    stats.low_priority_avg_wait_ms * (1.0 - alpha) + wait_ms * alpha;
            }
        }
    }

    async fn update_load_metrics(&self) {
        let (high_util, normal_util, low_util) = self.get_utilization().await;

        self.metrics.insert("qos.high_priority_utilization".to_string(), high_util);
        self.metrics.insert("qos.normal_priority_utilization".to_string(), normal_util);
        self.metrics.insert("qos.low_priority_utilization".to_string(), low_util);

        let stats = self.statistics.read().await;
        self.metrics.insert("qos.high_priority_latency_ms".to_string(),
                            stats.high_priority_avg_wait_ms);
        self.metrics.insert("qos.normal_priority_latency_ms".to_string(),
                            stats.normal_priority_avg_wait_ms);
        self.metrics.insert("qos.low_priority_latency_ms".to_string(),
                            stats.low_priority_avg_wait_ms);
    }

    async fn update_metrics(&self, stats: &QosStatistics) {
        let total_requests = stats.high_priority_requests +
            stats.normal_priority_requests +
            stats.low_priority_requests;

        if total_requests > 0 {
            let high_rate = stats.high_priority_requests as f64 / total_requests as f64;
            let normal_rate = stats.normal_priority_requests as f64 / total_requests as f64;
            let low_rate = stats.low_priority_requests as f64 / total_requests as f64;

            self.metrics.insert("qos.high_priority_rate".to_string(), high_rate);
            self.metrics.insert("qos.normal_priority_rate".to_string(), normal_rate);
            self.metrics.insert("qos.low_priority_rate".to_string(), low_rate);

            let rejection_rate = (stats.high_priority_rejected +
                stats.normal_priority_rejected +
                stats.low_priority_rejected) as f64 / total_requests as f64;
            self.metrics.insert("qos.rejection_rate".to_string(), rejection_rate);
        }
    }

    /// Динамическое обновление квот (public метод)
    pub async fn update_quotas(
        &self,
        high_priority: Option<f64>,
        normal_priority: Option<f64>,
        low_priority: Option<f64>,
    ) -> Result<(), QosError> {
        let quotas = self.quotas.read().await;

        let new_high = high_priority.unwrap_or(quotas.current_high_priority);
        let new_normal = normal_priority.unwrap_or(quotas.current_normal_priority);
        let new_low = low_priority.unwrap_or(quotas.current_low_priority);

        // Проверяем сумму
        let total = new_high + new_normal + new_low;
        if (total - 1.0).abs() > 0.01 {
            return Err(QosError::InvalidQuotas);
        }

        // Создаем решение для применения
        let decision = AdaptationDecision {
            timestamp: Instant::now(),
            from_high: quotas.current_high_priority,
            from_normal: quotas.current_normal_priority,
            from_low: quotas.current_low_priority,
            to_high: new_high,
            to_normal: new_normal,
            to_low: new_low,
            reason: "Manual quota update".to_string(),
            improvement_expected: 0.0,
        };

        // Применяем решение
        self.apply_adaptation_decision(decision).await?;

        info!("⚙️ QoS квоты обновлены вручную:");
        info!("  High: {:.1}%", new_high * 100.0);
        info!("  Normal: {:.1}%", new_normal * 100.0);
        info!("  Low: {:.1}%", new_low * 100.0);

        Ok(())
    }

    /// Получить статистику
    pub async fn get_statistics(&self) -> QosStatistics {
        self.statistics.read().await.clone()
    }

    /// Получить текущие квоты
    pub async fn get_quotas(&self) -> (f64, f64, f64) {
        let quotas = self.quotas.read().await;
        (quotas.current_high_priority, quotas.current_normal_priority, quotas.current_low_priority)
    }

    /// Получить использование по приоритетам
    pub async fn get_utilization(&self) -> (f64, f64, f64) {
        let high_available = self.semaphores.high_priority.available_permits();
        let normal_available = self.semaphores.normal_priority.available_permits();
        let low_available = self.semaphores.low_priority.available_permits();

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

impl AdaptationEngine {
    async fn record_load_sample(&self, analysis: &LoadAnalysis) {
        let mut history = self.load_history.write().await;

        // Создаем LoadSample из LoadAnalysis
        let load_sample = LoadSample {
            timestamp: analysis.timestamp,
            high_load: analysis.high_utilization,
            normal_load: analysis.normal_utilization,
            low_load: analysis.low_utilization,
            high_rejection_rate: analysis.high_rejection_rate,
            normal_rejection_rate: analysis.normal_rejection_rate,
            low_rejection_rate: analysis.low_rejection_rate,
            system_load: analysis.system_load,
        };

        history.push(load_sample);

        if history.len() > self.max_history_size {
            history.remove(0);
        }
    }

    async fn record_decision(&self, decision: AdaptationDecision) {
        let mut history = self.decision_history.write().await;
        history.push(decision);

        if history.len() > self.max_history_size {
            history.remove(0);
        }
    }
}

/// Разрешение QoS
pub struct QosPermit<'a> {
    priority: Priority,
    _permit: tokio::sync::SemaphorePermit<'a>,
    manager: &'a QosManager,
}

impl<'a> QosPermit<'a> {
    pub fn priority(&self) -> Priority {
        self.priority
    }
}

impl<'a> Drop for QosPermit<'a> {
    fn drop(&mut self) {
        // Автоматически освобождаем разрешение при выходе из scope
        self.manager.release_permit(self.priority);
    }
}

#[derive(Debug, Clone)]
pub struct QosSystemStatus {
    pub current_quotas: (f64, f64, f64),
    pub base_quotas: (f64, f64, f64),
    pub utilization: (f64, f64, f64),
    pub latency_ms: (f64, f64, f64),
    pub rejection_rates: (f64, f64, f64),
    pub last_adaptation: Instant,
}

#[derive(Debug, Clone)]
pub struct PolicyUpdate {
    pub enabled: Option<bool>,
    pub adaptation_interval: Option<Duration>,
    pub target_high_priority_latency_ms: Option<f64>,
    pub target_normal_priority_latency_ms: Option<f64>,
    pub max_quota_change_per_step: Option<f64>,
    pub min_quota_per_class: Option<f64>,
    pub starvation_prevention_enabled: Option<bool>,
}

#[derive(Debug, thiserror::Error)]
pub enum QosError {
    #[error("Таймаут ожидания разрешения QoS")]
    Timeout,
    #[error("Семафор QoS закрыт")]
    SemaphoreClosed,
    #[error("Неверные квоты QoS")]
    InvalidQuotas,
    #[error("Адаптация QoS отключена")]
    AdaptationDisabled,
    #[error("Слишком частая адаптация QoS")]
    TooFrequentAdaptation,
}

// Исправленная реализация Clone без блокировки
impl Clone for QosManager {
    fn clone(&self) -> Self {
        // Берем текущие квоты без блокировки (используем значения по умолчанию)
        let high_quota = 0.4_f64;
        let normal_quota = 0.4_f64;
        let low_quota = 0.2_f64;
        let total_capacity = 100000;

        // Создаем базовый менеджер БЕЗ запуска фоновых задач
        let quotas = QosQuotas {
            base_high_priority: high_quota,
            base_normal_priority: normal_quota,
            base_low_priority: low_quota,
            current_high_priority: high_quota,
            current_normal_priority: normal_quota,
            current_low_priority: low_quota,
            total_capacity,
            last_adaptation: Instant::now(),
        };

        let high_capacity = (total_capacity as f64 * high_quota).ceil() as usize;
        let normal_capacity = (total_capacity as f64 * normal_quota).ceil() as usize;
        let low_capacity = (total_capacity as f64 * low_quota).ceil() as usize;

        let policy = DynamicPolicy::default();

        let adaptation_engine = Arc::new(AdaptationEngine {
            load_history: RwLock::new(Vec::new()),
            decision_history: RwLock::new(Vec::new()),
            max_history_size: 100,
        });

        Self {
            quotas: RwLock::new(quotas),
            semaphores: QosSemaphores {
                high_priority: Semaphore::new(high_capacity),
                normal_priority: Semaphore::new(normal_capacity),
                low_priority: Semaphore::new(low_capacity),
            },
            metrics: Arc::new(DashMap::new()),
            statistics: RwLock::new(QosStatistics {
                high_priority_requests: 0,
                normal_priority_requests: 0,
                low_priority_requests: 0,
                high_priority_rejected: 0,
                normal_priority_rejected: 0,
                low_priority_rejected: 0,
                high_priority_wait_time: Duration::from_secs(0),
                normal_priority_wait_time: Duration::from_secs(0),
                low_priority_wait_time: Duration::from_secs(0),
                high_priority_avg_wait_ms: 0.0,
                normal_priority_avg_wait_ms: 0.0,
                low_priority_avg_wait_ms: 0.0,
            }),
            dynamic_policy: Mutex::new(policy),
            adaptation_engine: adaptation_engine.clone(),
            // НЕ запускаем фоновые задачи в клоне!
        }
    }
}