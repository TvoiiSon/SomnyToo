use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use crate::core::protocol::batch_system::core::buffer::UnifiedBufferPool;
use crate::core::protocol::batch_system::core::processor::CryptoProcessor;
use crate::core::protocol::batch_system::optimized::work_stealing_dispatcher::WorkStealingDispatcher;
use crate::core::protocol::batch_system::optimized::crypto_processor::OptimizedCryptoProcessor;
use crate::core::protocol::batch_system::optimized::buffer_pool::OptimizedBufferPool;
use crate::core::monitoring::unified_monitor::{UnifiedMonitor, AlertLevel, Monitor, MonitorMetrics, MetricValue};

#[derive(Debug, Clone)]
pub struct BatchSystemMetrics {
    pub buffer_pool_hit_rate: f64,
    pub buffer_pool_reuse_rate: f64,
    pub crypto_operations_success_rate: f64,
    pub work_stealing_tasks: u64,
    pub active_connections: usize,
    pub pending_tasks: usize,
    pub total_data_processed: u64,
    pub avg_processing_time_ms: f64,
    pub batch_processing_rate: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentHealth {
    Healthy,
    Degraded,
    Unhealthy,
    Critical,
}

#[derive(Debug, Clone)]
pub struct ComponentStatus {
    pub name: String,
    pub health: ComponentHealth,
    pub last_check: SystemTime,
    pub metrics: HashMap<String, MetricValue>,
}

#[derive(Clone)]
pub struct BatchSystemMonitor {
    buffer_pool: Option<Arc<UnifiedBufferPool>>,
    optimized_buffer_pool: Option<Arc<OptimizedBufferPool>>,
    crypto_processor: Option<Arc<CryptoProcessor>>,
    optimized_crypto_processor: Option<Arc<OptimizedCryptoProcessor>>,
    work_stealing_dispatcher: Option<Arc<WorkStealingDispatcher>>,
    unified_monitor: Option<Arc<UnifiedMonitor>>,
    component_statuses: Arc<RwLock<HashMap<String, ComponentStatus>>>,
    metrics_history: Arc<RwLock<Vec<(SystemTime, BatchSystemMetrics)>>>,
    last_collection_time: Arc<RwLock<SystemTime>>,
}

impl BatchSystemMonitor {
    pub fn new() -> Self {
        Self {
            buffer_pool: None,
            optimized_buffer_pool: None,
            crypto_processor: None,
            optimized_crypto_processor: None,
            work_stealing_dispatcher: None,
            unified_monitor: None,
            component_statuses: Arc::new(RwLock::new(HashMap::new())),
            metrics_history: Arc::new(RwLock::new(Vec::new())),
            last_collection_time: Arc::new(RwLock::new(SystemTime::now())),
        }
    }

    pub fn attach_buffer_pool(&mut self, buffer_pool: Arc<UnifiedBufferPool>) {
        self.buffer_pool = Some(buffer_pool);
    }

    pub fn attach_optimized_buffer_pool(&mut self, buffer_pool: Arc<OptimizedBufferPool>) {
        self.optimized_buffer_pool = Some(buffer_pool);
    }

    pub fn attach_crypto_processor(&mut self, processor: Arc<CryptoProcessor>) {
        self.crypto_processor = Some(processor);
    }

    pub fn attach_optimized_crypto_processor(&mut self, processor: Arc<OptimizedCryptoProcessor>) {
        self.optimized_crypto_processor = Some(processor);
    }

    pub fn attach_work_stealing_dispatcher(&mut self, dispatcher: Arc<WorkStealingDispatcher>) {
        self.work_stealing_dispatcher = Some(dispatcher);
    }

    pub fn attach_unified_monitor(&mut self, monitor: Arc<UnifiedMonitor>) {
        self.unified_monitor = Some(monitor);
    }

    pub async fn collect_metrics(&self) -> BatchSystemMetrics {
        let mut metrics = BatchSystemMetrics {
            buffer_pool_hit_rate: 0.0,
            buffer_pool_reuse_rate: 0.0,
            crypto_operations_success_rate: 0.0,
            work_stealing_tasks: 0,
            active_connections: 0,
            pending_tasks: 0,
            total_data_processed: 0,
            avg_processing_time_ms: 0.0,
            batch_processing_rate: 0.0,
        };

        // Собираем метрики из буферных пулов
        if let Some(ref buffer_pool) = self.buffer_pool {
            let stats = buffer_pool.get_stats();
            if stats.allocation_count + stats.reuse_count > 0 {
                metrics.buffer_pool_hit_rate = stats.reuse_count as f64 /
                    (stats.allocation_count + stats.reuse_count) as f64;
            }
        }

        if let Some(ref optimized_pool) = self.optimized_buffer_pool {
            metrics.buffer_pool_reuse_rate = optimized_pool.get_reuse_rate();
        }

        // Собираем метрики из криптопроцессоров
        if let Some(ref crypto_processor) = self.crypto_processor {
            let stats = crypto_processor.get_stats();
            if stats.total_operations > 0 {
                metrics.crypto_operations_success_rate = 1.0 -
                    (stats.total_failed as f64 / stats.total_operations as f64);
            }
        }

        // Собираем метрики из диспетчеров
        if let Some(ref dispatcher) = self.work_stealing_dispatcher {
            let stats = dispatcher.get_stats();
            metrics.work_stealing_tasks = stats.values().sum();
        }

        // Сохраняем историю метрик
        let mut history = self.metrics_history.write().await;
        history.push((SystemTime::now(), metrics.clone()));

        // Ограничиваем размер истории
        if history.len() > 1000 {
            history.remove(0);
        }

        *self.last_collection_time.write().await = SystemTime::now();

        metrics
    }

    pub async fn check_components_health(&self) -> HashMap<String, ComponentHealth> {
        let mut health_statuses = HashMap::new();
        let mut component_metrics = HashMap::new();

        // Проверяем буферный пул
        if let (Some(buffer_pool), Some(optimized_pool)) =
            (&self.buffer_pool, &self.optimized_buffer_pool)
        {
            let stats = buffer_pool.get_stats();
            let reuse_rate = optimized_pool.get_reuse_rate();

            let hit_rate = if stats.allocation_count + stats.reuse_count > 0 {
                stats.reuse_count as f64 / (stats.allocation_count + stats.reuse_count) as f64
            } else {
                0.0
            };

            let health = if stats.total_allocated == 0 {
                // Система только запустилась
                ComponentHealth::Healthy
            } else if hit_rate > 0.7 && reuse_rate > 0.6 {
                ComponentHealth::Healthy
            } else if hit_rate > 0.5 && reuse_rate > 0.4 {
                ComponentHealth::Degraded
            } else {
                ComponentHealth::Unhealthy
            };

            health_statuses.insert("buffer_pool".to_string(), health);

            let mut metrics = HashMap::new();
            metrics.insert("hit_rate".to_string(), MetricValue::Gauge(hit_rate));
            metrics.insert("reuse_rate".to_string(), MetricValue::Gauge(reuse_rate));
            metrics.insert("allocations".to_string(), MetricValue::Counter(stats.allocation_count));
            metrics.insert("reuses".to_string(), MetricValue::Counter(stats.reuse_count));

            component_metrics.insert("buffer_pool".to_string(), metrics);
        }

        // Проверяем криптопроцессор
        if let Some(ref crypto_processor) = self.crypto_processor {
            let stats = crypto_processor.get_stats();
            let success_rate = if stats.total_operations > 0 {
                1.0 - (stats.total_failed as f64 / stats.total_operations as f64)
            } else {
                1.0
            };

            let health = if success_rate > 0.99 {
                ComponentHealth::Healthy
            } else if success_rate > 0.95 {
                ComponentHealth::Degraded
            } else {
                ComponentHealth::Unhealthy
            };

            health_statuses.insert("crypto_processor".to_string(), health);

            let mut metrics = HashMap::new();
            metrics.insert("success_rate".to_string(), MetricValue::Gauge(success_rate));
            metrics.insert("total_operations".to_string(), MetricValue::Counter(stats.total_operations));
            metrics.insert("failed_operations".to_string(), MetricValue::Counter(stats.total_failed));

            component_metrics.insert("crypto_processor".to_string(), metrics);
        }

        // Проверяем диспетчеры
        if let Some(ref dispatcher) = self.work_stealing_dispatcher {
            let stats = dispatcher.get_stats();
            let total_tasks: u64 = stats.values().sum();

            let health = if total_tasks > 0 {
                ComponentHealth::Healthy
            } else {
                ComponentHealth::Degraded
            };

            health_statuses.insert("dispatchers".to_string(), health);

            let mut metrics = HashMap::new();
            metrics.insert("total_tasks".to_string(), MetricValue::Counter(total_tasks));

            component_metrics.insert("dispatchers".to_string(), metrics);
        }

        // Обновляем статусы компонентов
        let mut statuses = self.component_statuses.write().await;
        for (name, health) in &health_statuses {
            statuses.insert(name.clone(), ComponentStatus {
                name: name.clone(),
                health: *health,
                last_check: SystemTime::now(),
                metrics: component_metrics.get(name).cloned().unwrap_or_default(),
            });
        }

        health_statuses
    }

    pub async fn send_alerts(&self) {
        let health_statuses = self.check_components_health().await;

        if let Some(ref unified_monitor) = self.unified_monitor {
            for (component, health) in health_statuses {
                match health {
                    ComponentHealth::Unhealthy | ComponentHealth::Critical => {
                        unified_monitor.add_alert(
                            AlertLevel::Error,
                            "batch_system",
                            &format!("Component {} is {}", component,
                                     if health == ComponentHealth::Critical { "critical" } else { "unhealthy" })
                        ).await;
                    }
                    ComponentHealth::Degraded => {
                        unified_monitor.add_alert(
                            AlertLevel::Warning,
                            "batch_system",
                            &format!("Component {} is degraded", component)
                        ).await;
                    }
                    ComponentHealth::Healthy => {
                        // Не отправляем алерты для здоровых компонентов
                    }
                }
            }
        }
    }

    pub async fn get_component_status(&self, component: &str) -> Option<ComponentStatus> {
        let statuses = self.component_statuses.read().await;
        statuses.get(component).cloned()
    }

    pub async fn get_all_statuses(&self) -> HashMap<String, ComponentStatus> {
        let statuses = self.component_statuses.read().await;
        statuses.clone()
    }

    pub async fn get_metrics_history(&self, limit: usize) -> Vec<(SystemTime, BatchSystemMetrics)> {
        let history = self.metrics_history.read().await;
        let start = if history.len() > limit {
            history.len() - limit
        } else {
            0
        };
        history[start..].to_vec()
    }

    pub async fn reset_metrics(&self) {
        let mut history = self.metrics_history.write().await;
        history.clear();

        let mut statuses = self.component_statuses.write().await;
        statuses.clear();
    }

    pub async fn health_check(&self) -> bool {
        let health_statuses = self.check_components_health().await;

        // Система считается здоровой, если нет критических или нездоровых компонентов
        !health_statuses.values().any(|h|
            *h == ComponentHealth::Unhealthy || *h == ComponentHealth::Critical
        )
    }
}

#[async_trait::async_trait]
impl Monitor for BatchSystemMonitor {
    fn name(&self) -> &'static str {
        "batch_system"
    }

    async fn collect_metrics(&self) -> MonitorMetrics {
        let metrics = self.collect_metrics().await;
        let health = self.health_check().await;

        let mut metric_map = HashMap::new();
        metric_map.insert("buffer_pool_hit_rate".to_string(),
                          MetricValue::Gauge(metrics.buffer_pool_hit_rate));
        metric_map.insert("buffer_pool_reuse_rate".to_string(),
                          MetricValue::Gauge(metrics.buffer_pool_reuse_rate));
        metric_map.insert("crypto_operations_success_rate".to_string(),
                          MetricValue::Gauge(metrics.crypto_operations_success_rate));
        metric_map.insert("work_stealing_tasks".to_string(),
                          MetricValue::Gauge(metrics.work_stealing_tasks as f64));
        metric_map.insert("active_connections".to_string(),
                          MetricValue::Gauge(metrics.active_connections as f64));
        metric_map.insert("pending_tasks".to_string(),
                          MetricValue::Gauge(metrics.pending_tasks as f64));
        metric_map.insert("total_data_processed".to_string(),
                          MetricValue::Counter(metrics.total_data_processed));
        metric_map.insert("avg_processing_time_ms".to_string(),
                          MetricValue::Gauge(metrics.avg_processing_time_ms));
        metric_map.insert("batch_processing_rate".to_string(),
                          MetricValue::Gauge(metrics.batch_processing_rate));

        MonitorMetrics {
            name: self.name().to_string(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            metrics: metric_map,
            health,
        }
    }

    async fn health_check(&self) -> bool {
        self.health_check().await
    }

    async fn reset_metrics(&self) {
        self.reset_metrics().await;
    }
}