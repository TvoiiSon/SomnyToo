use std::sync::Arc;
use std::time::{Duration};
use tokio::sync::{RwLock, Semaphore};
use tracing::info;
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
struct QosQuotas {
    high_priority: f64,
    normal_priority: f64,
    low_priority: f64,
    total_capacity: usize,
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
}

impl QosManager {
    pub fn new(
        high_priority_quota: f64,
        normal_priority_quota: f64,
        low_priority_quota: f64,
        total_capacity: usize,
    ) -> Self {
        // Проверка квот
        assert!((high_priority_quota + normal_priority_quota + low_priority_quota - 1.0).abs() < 0.01,
                "QoS квоты должны суммироваться в 1.0");

        let quotas = QosQuotas {
            high_priority: high_priority_quota,
            normal_priority: normal_priority_quota,
            low_priority: low_priority_quota,
            total_capacity,
        };

        let high_capacity = (total_capacity as f64 * high_priority_quota).ceil() as usize;
        let normal_capacity = (total_capacity as f64 * normal_priority_quota).ceil() as usize;
        let low_capacity = (total_capacity as f64 * low_priority_quota).ceil() as usize;

        info!("🚦 QoS Manager инициализирован:");
        info!("  - Высокий приоритет: {} permits ({}%)", high_capacity, high_priority_quota * 100.0);
        info!("  - Нормальный приоритет: {} permits ({}%)", normal_capacity, normal_priority_quota * 100.0);
        info!("  - Низкий приоритет: {} permits ({}%)", low_capacity, low_priority_quota * 100.0);

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
            }),
        }
    }

    /// Получить разрешение на обработку по приоритету
    pub async fn acquire_permit(&self, priority: Priority) -> Result<QosPermit<'_>, QosError> {
        let semaphore = match priority {
            Priority::Critical | Priority::High => &self.semaphores.high_priority,
            Priority::Normal => &self.semaphores.normal_priority,
            Priority::Low | Priority::Background => &self.semaphores.low_priority,
        };

        // Обновляем статистику
        self.update_statistics(priority, false);

        // Пытаемся получить разрешение с таймаутом
        let permit = match tokio::time::timeout(Duration::from_millis(100), semaphore.acquire()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                self.update_statistics(priority, true);
                return Err(QosError::SemaphoreClosed);
            }
            Err(_) => {
                self.update_statistics(priority, true);
                return Err(QosError::Timeout);
            }
        };

        Ok(QosPermit {
            priority,
            _permit: permit,
        })
    }

    /// Динамическое обновление квот
    pub async fn update_quotas(
        &self,
        high_priority: Option<f64>,
        normal_priority: Option<f64>,
        low_priority: Option<f64>,
    ) -> Result<(), QosError> {
        let mut quotas = self.quotas.write().await;

        let new_high = high_priority.unwrap_or(quotas.high_priority);
        let new_normal = normal_priority.unwrap_or(quotas.normal_priority);
        let new_low = low_priority.unwrap_or(quotas.low_priority);

        // Проверяем сумму
        if (new_high + new_normal + new_low - 1.0).abs() > 0.01 {
            return Err(QosError::InvalidQuotas);
        }

        // Обновляем квоты
        quotas.high_priority = new_high;
        quotas.normal_priority = new_normal;
        quotas.low_priority = new_low;

        // Пересчитываем емкости
        let high_capacity = (quotas.total_capacity as f64 * new_high).ceil() as usize;
        let normal_capacity = (quotas.total_capacity as f64 * new_normal).ceil() as usize;
        let low_capacity = (quotas.total_capacity as f64 * new_low).ceil() as usize;

        // Увеличиваем/уменьшаем семафоры
        self.adjust_semaphore(&self.semaphores.high_priority, high_capacity);
        self.adjust_semaphore(&self.semaphores.normal_priority, normal_capacity);
        self.adjust_semaphore(&self.semaphores.low_priority, low_capacity);

        info!("🔄 QoS квоты обновлены: High={}%, Normal={}%, Low={}%",
              new_high * 100.0, new_normal * 100.0, new_low * 100.0);

        Ok(())
    }

    fn adjust_semaphore(&self, semaphore: &Semaphore, new_capacity: usize) {
        let current_capacity = semaphore.available_permits();

        if new_capacity > current_capacity {
            semaphore.add_permits(new_capacity - current_capacity);
        } else if new_capacity < current_capacity {
            // Для уменьшения нужно более сложная логика
        }
    }

    fn update_statistics(&self, priority: Priority, rejected: bool) {
        let mut stats = self.statistics.blocking_write();

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
        self.update_metrics(&stats);
    }

    fn update_metrics(&self, stats: &QosStatistics) {
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

    /// Получить статистику
    pub async fn get_statistics(&self) -> QosStatistics {
        self.statistics.read().await.clone()
    }

    /// Сбросить статистику
    pub async fn reset_statistics(&self) {
        let mut stats = self.statistics.write().await;
        *stats = QosStatistics {
            high_priority_requests: 0,
            normal_priority_requests: 0,
            low_priority_requests: 0,
            high_priority_rejected: 0,
            normal_priority_rejected: 0,
            low_priority_rejected: 0,
        };
    }

    /// Получить текущие квоты
    pub async fn get_quotas(&self) -> (f64, f64, f64) {
        let quotas = self.quotas.read().await;
        (quotas.high_priority, quotas.normal_priority, quotas.low_priority)
    }

    /// Получить использование по приоритетам
    pub async fn get_utilization(&self) -> (f64, f64, f64) {
        let high_available = self.semaphores.high_priority.available_permits();
        let normal_available = self.semaphores.normal_priority.available_permits();
        let low_available = self.semaphores.low_priority.available_permits();

        let quotas = self.quotas.read().await;
        let high_capacity = (quotas.total_capacity as f64 * quotas.high_priority).ceil() as usize;
        let normal_capacity = (quotas.total_capacity as f64 * quotas.normal_priority).ceil() as usize;
        let low_capacity = (quotas.total_capacity as f64 * quotas.low_priority).ceil() as usize;

        (
            1.0 - (high_available as f64 / high_capacity as f64),
            1.0 - (normal_available as f64 / normal_capacity as f64),
            1.0 - (low_available as f64 / low_capacity as f64),
        )
    }
}

/// Разрешение QoS
pub struct QosPermit<'a> {
    priority: Priority,
    _permit: tokio::sync::SemaphorePermit<'a>,
}

impl<'a> QosPermit<'a> {
    pub fn priority(&self) -> Priority {
        self.priority
    }
}

#[derive(Debug, thiserror::Error)]
pub enum QosError {
    #[error("Таймаут ожидания разрешения QoS")]
    Timeout,
    #[error("Семафор QoS закрыт")]
    SemaphoreClosed,
    #[error("Неверные квоты QoS")]
    InvalidQuotas,
}