use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore};
use tracing::{info, warn};
use dashmap::DashMap;

use crate::core::protocol::batch_system::types::priority::Priority;

/// QoS Manager для приоритизации трафика
pub struct QosManager {
    quotas: RwLock<QosQuotas>,
    semaphores: QosSemaphores,
    metrics: Arc<DashMap<String, f64>>,
    statistics: RwLock<QosStatistics>,
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
    pub high_priority_avg_wait_ms: f64,
    pub normal_priority_avg_wait_ms: f64,
    pub low_priority_avg_wait_ms: f64,
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
        }
    }
}

/// Решение об адаптации
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
}

/// Вспомогательная функция для преобразования приоритета в строку
fn priority_to_str(priority: Priority) -> &'static str {
    match priority {
        Priority::Critical | Priority::High => "high",
        Priority::Normal => "normal",
        Priority::Low | Priority::Background => "low",
    }
}

impl QosManager {
    pub fn new(
        high_priority_quota: f64,
        normal_priority_quota: f64,
        low_priority_quota: f64,
        total_capacity: usize,
    ) -> Self {
        let total = high_priority_quota + normal_priority_quota + low_priority_quota;
        let (normalized_high, normalized_normal, normalized_low) = if (total - 1.0).abs() > 0.01 {
            warn!("⚠️ QoS quotas don't sum to 1.0 ({}), normalizing", total);
            (
                high_priority_quota / total,
                normal_priority_quota / total,
                low_priority_quota / total,
            )
        } else {
            (high_priority_quota, normal_priority_quota, low_priority_quota)
        };

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

        info!("🚦 QoS Manager initialized:");
        info!("  - High: {} permits ({:.1}%)", high_capacity, normalized_high * 100.0);
        info!("  - Normal: {} permits ({:.1}%)", normal_capacity, normalized_normal * 100.0);
        info!("  - Low: {} permits ({:.1}%)", low_capacity, normalized_low * 100.0);
        info!("  - Total capacity: {}", total_capacity);

        let metrics = Arc::new(DashMap::new());

        metrics.insert("qos.initialized".to_string(), 1.0);
        metrics.insert("qos.high_capacity".to_string(), high_capacity as f64);
        metrics.insert("qos.normal_capacity".to_string(), normal_capacity as f64);
        metrics.insert("qos.low_capacity".to_string(), low_capacity as f64);

        Self {
            quotas: RwLock::new(quotas),
            semaphores: QosSemaphores {
                high_priority: Semaphore::new(high_capacity),
                normal_priority: Semaphore::new(normal_capacity),
                low_priority: Semaphore::new(low_capacity),
            },
            metrics,
            statistics: RwLock::new(QosStatistics::default()),
        }
    }

    /// Получить разрешение на обработку по приоритету
    pub async fn acquire_permit(&self, priority: Priority) -> Result<QosPermit<'_>, QosError> {
        let start_wait = Instant::now();

        self.update_statistics(priority, false).await;

        let permit_result = match priority {
            Priority::Critical | Priority::High => {
                tokio::time::timeout(
                    Duration::from_millis(100),
                    self.semaphores.high_priority.acquire()
                ).await
            }
            Priority::Normal => {
                tokio::time::timeout(
                    Duration::from_millis(200),
                    self.semaphores.normal_priority.acquire()
                ).await
            }
            Priority::Low | Priority::Background => {
                tokio::time::timeout(
                    Duration::from_millis(500),
                    self.semaphores.low_priority.acquire()
                ).await
            }
        };

        match permit_result {
            Ok(Ok(permit_owned)) => {
                let wait_time = start_wait.elapsed();
                self.record_wait_time(priority, wait_time).await;

                self.record_metric(
                    format!("qos.acquire_success.{}", priority_to_str(priority)),
                    1.0
                );
                self.record_metric(
                    format!("qos.{}_wait_ms", priority_to_str(priority)),
                    wait_time.as_millis() as f64
                );

                Ok(QosPermit {
                    priority,
                    manager: self,
                    _permit: Some(permit_owned),
                })
            }
            Ok(Err(_)) => {
                self.update_statistics(priority, true).await;
                self.record_metric(
                    format!("qos.acquire_failed.{}", priority_to_str(priority)),
                    1.0
                );
                self.record_metric(
                    format!("qos.{}_rejected", priority_to_str(priority)),
                    1.0
                );
                Err(QosError::SemaphoreClosed)
            }
            Err(_) => {
                self.update_statistics(priority, true).await;
                self.record_metric(
                    format!("qos.acquire_timeout.{}", priority_to_str(priority)),
                    1.0
                );
                self.record_metric(
                    format!("qos.{}_timeout", priority_to_str(priority)),
                    1.0
                );
                Err(QosError::Timeout)
            }
        }
    }

    /// Освобождение разрешения
    fn release_permit(&self, priority: Priority) {
        match priority {
            Priority::Critical | Priority::High => self.semaphores.high_priority.add_permits(1),
            Priority::Normal => self.semaphores.normal_priority.add_permits(1),
            Priority::Low | Priority::Background => self.semaphores.low_priority.add_permits(1),
        }

        self.record_metric(
            format!("qos.{}_released", priority_to_str(priority)),
            1.0
        );
    }

    /// Динамическое обновление квот
    pub async fn adapt_quotas(&self) -> Result<AdaptationDecision, QosError> {
        let quotas = self.quotas.read().await;
        let stats = self.statistics.read().await;

        let total_requests = stats.high_priority_requests +
            stats.normal_priority_requests +
            stats.low_priority_requests;

        if total_requests < 100 {
            return Err(QosError::InsufficientData);
        }

        let high_rejection = if stats.high_priority_requests > 0 {
            stats.high_priority_rejected as f64 / stats.high_priority_requests as f64
        } else { 0.0 };

        let normal_rejection = if stats.normal_priority_requests > 0 {
            stats.normal_priority_rejected as f64 / stats.normal_priority_requests as f64
        } else { 0.0 };

        let low_rejection = if stats.low_priority_requests > 0 {
            stats.low_priority_rejected as f64 / stats.low_priority_requests as f64
        } else { 0.0 };

        let mut new_high = quotas.current_high_priority;
        let mut new_normal = quotas.current_normal_priority;
        let mut new_low = quotas.current_low_priority;
        let mut reason = String::new();

        // Адаптивная логика
        if high_rejection > 0.1 {
            let increase = 0.05;
            if new_low > 0.1 {
                new_low -= increase;
                new_high += increase;
                reason = format!("High priority rejection {:.1}%, taking from low", high_rejection * 100.0);
            }
        } else if normal_rejection > 0.2 {
            let increase = 0.03;
            if new_low > 0.1 {
                new_low -= increase;
                new_normal += increase;
                reason = format!("Normal priority rejection {:.1}%, taking from low", normal_rejection * 100.0);
            }
        } else if low_rejection > 0.3 && quotas.current_low_priority < quotas.base_low_priority * 1.2 {
            let increase = 0.02;
            if new_high > 0.2 {
                new_high -= increase;
                new_low += increase;
                reason = format!("Low priority starvation, increasing quota");
            }
        }

        if reason.is_empty() {
            return Err(QosError::NoAdaptationNeeded);
        }

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
        };

        drop(quotas);
        self.apply_adaptation(&decision).await?;
        Ok(decision)
    }

    async fn apply_adaptation(&self, decision: &AdaptationDecision) -> Result<(), QosError> {
        let mut quotas = self.quotas.write().await;

        let high_capacity = (quotas.total_capacity as f64 * decision.to_high).ceil() as usize;
        let normal_capacity = (quotas.total_capacity as f64 * decision.to_normal).ceil() as usize;
        let low_capacity = (quotas.total_capacity as f64 * decision.to_low).ceil() as usize;

        let high_available = self.semaphores.high_priority.available_permits();
        let normal_available = self.semaphores.normal_priority.available_permits();
        let low_available = self.semaphores.low_priority.available_permits();

        let high_change = high_capacity as isize - high_available as isize;
        let normal_change = normal_capacity as isize - normal_available as isize;
        let low_change = low_capacity as isize - low_available as isize;

        if high_change > 0 {
            self.semaphores.high_priority.add_permits(high_change as usize);
        }
        if normal_change > 0 {
            self.semaphores.normal_priority.add_permits(normal_change as usize);
        }
        if low_change > 0 {
            self.semaphores.low_priority.add_permits(low_change as usize);
        }

        quotas.current_high_priority = decision.to_high;
        quotas.current_normal_priority = decision.to_normal;
        quotas.current_low_priority = decision.to_low;
        quotas.last_adaptation = Instant::now();

        self.record_metric("qos.adaptation".to_string(), 1.0);
        self.record_metric("qos.high_quota".to_string(), decision.to_high);
        self.record_metric("qos.normal_quota".to_string(), decision.to_normal);
        self.record_metric("qos.low_quota".to_string(), decision.to_low);

        info!("🔄 QoS adaptation applied:");
        info!("  High: {:.1}% → {:.1}%", decision.from_high * 100.0, decision.to_high * 100.0);
        info!("  Normal: {:.1}% → {:.1}%", decision.from_normal * 100.0, decision.to_normal * 100.0);
        info!("  Low: {:.1}% → {:.1}%", decision.from_low * 100.0, decision.to_low * 100.0);
        info!("  Reason: {}", decision.reason);

        Ok(())
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
    }

    async fn record_wait_time(&self, priority: Priority, wait_time: Duration) {
        let mut stats = self.statistics.write().await;
        let alpha = 0.1;
        let wait_ms = wait_time.as_millis() as f64;

        match priority {
            Priority::Critical | Priority::High => {
                stats.high_priority_avg_wait_ms =
                    stats.high_priority_avg_wait_ms * (1.0 - alpha) + wait_ms * alpha;

                self.record_metric("qos.high_avg_wait_ms".to_string(),
                                   stats.high_priority_avg_wait_ms);
            }
            Priority::Normal => {
                stats.normal_priority_avg_wait_ms =
                    stats.normal_priority_avg_wait_ms * (1.0 - alpha) + wait_ms * alpha;

                self.record_metric("qos.normal_avg_wait_ms".to_string(),
                                   stats.normal_priority_avg_wait_ms);
            }
            Priority::Low | Priority::Background => {
                stats.low_priority_avg_wait_ms =
                    stats.low_priority_avg_wait_ms * (1.0 - alpha) + wait_ms * alpha;

                self.record_metric("qos.low_avg_wait_ms".to_string(),
                                   stats.low_priority_avg_wait_ms);
            }
        }
    }

    /// Запись метрик
    fn record_metric(&self, key: String, value: f64) {
        self.metrics.insert(key, value);
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

        let high_util = if high_capacity > 0 { 1.0 - (high_available as f64 / high_capacity as f64) } else { 0.0 };
        let normal_util = if normal_capacity > 0 { 1.0 - (normal_available as f64 / normal_capacity as f64) } else { 0.0 };
        let low_util = if low_capacity > 0 { 1.0 - (low_available as f64 / low_capacity as f64) } else { 0.0 };

        (high_util, normal_util, low_util)
    }

    /// Получить все метрики
    pub fn get_all_metrics(&self) -> std::collections::HashMap<String, f64> {
        let mut result = std::collections::HashMap::new();
        for entry in self.metrics.iter() {
            result.insert(entry.key().clone(), *entry.value());
        }
        result
    }

    /// Получить конкретную метрику
    pub fn get_metric(&self, key: &str) -> Option<f64> {
        self.metrics.get(key).map(|m| *m.value())
    }
}

/// Разрешение QoS
pub struct QosPermit<'a> {
    priority: Priority,
    manager: &'a QosManager,
    _permit: Option<tokio::sync::SemaphorePermit<'a>>,
}

impl<'a> QosPermit<'a> {
    pub fn priority(&self) -> Priority {
        self.priority
    }
}

impl<'a> Drop for QosPermit<'a> {
    fn drop(&mut self) {
        self.manager.release_permit(self.priority);
    }
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
}

impl Clone for QosManager {
    fn clone(&self) -> Self {
        let quotas = self.quotas.try_read()
            .map(|q| q.clone())
            .unwrap_or_else(|_| {
                QosQuotas {
                    base_high_priority: 0.4,
                    base_normal_priority: 0.4,
                    base_low_priority: 0.2,
                    current_high_priority: 0.4,
                    current_normal_priority: 0.4,
                    current_low_priority: 0.2,
                    total_capacity: 100000,
                    last_adaptation: Instant::now(),
                }
            });

        let high_capacity = (quotas.total_capacity as f64 * quotas.current_high_priority).ceil() as usize;
        let normal_capacity = (quotas.total_capacity as f64 * quotas.current_normal_priority).ceil() as usize;
        let low_capacity = (quotas.total_capacity as f64 * quotas.current_low_priority).ceil() as usize;

        let metrics = Arc::new(DashMap::new());
        for entry in self.metrics.iter() {
            metrics.insert(entry.key().clone(), *entry.value());
        }

        Self {
            quotas: RwLock::new(quotas),
            semaphores: QosSemaphores {
                high_priority: Semaphore::new(high_capacity),
                normal_priority: Semaphore::new(normal_capacity),
                low_priority: Semaphore::new(low_capacity),
            },
            metrics,
            statistics: RwLock::new(QosStatistics::default()),
        }
    }
}