use std::sync::Arc;
use std::time::Duration;

use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::batch_system::optimized::work_stealing_dispatcher::WorkStealingDispatcher;
use crate::core::protocol::batch_system::optimized::buffer_pool::OptimizedBufferPool;
use crate::core::protocol::batch_system::optimized::crypto_processor::OptimizedCryptoProcessor;
use crate::core::protocol::batch_system::adaptive_batcher::AdaptiveBatcher;
use crate::core::protocol::batch_system::qos_manager::QosManager;
use crate::core::protocol::batch_system::circuit_breaker::CircuitBreaker;

/// Модель оптимизации создания компонентов
#[derive(Debug, Clone)]
pub struct FactoryOptimizationModel {
    /// Веса компонентов для аллокации ресурсов
    pub component_weights: std::collections::HashMap<String, f64>,

    /// Оптимальное количество воркеров для диспетчера
    pub optimal_dispatcher_workers: usize,

    /// Оптимальное количество крипто-воркеров
    pub optimal_crypto_workers: usize,

    /// Оптимальный размер буферного пула
    pub optimal_buffer_pool_size: usize,

    /// Целевая задержка
    pub target_latency: Duration,

    /// Максимальная пропускная способность
    pub max_throughput: f64,

    /// Коэффициент использования ресурсов
    pub resource_utilization: f64,
}

impl FactoryOptimizationModel {
    pub fn new() -> Self {
        let mut weights = std::collections::HashMap::new();
        weights.insert("dispatcher".to_string(), 0.35);
        weights.insert("crypto".to_string(), 0.30);
        weights.insert("buffer".to_string(), 0.20);
        weights.insert("batcher".to_string(), 0.10);
        weights.insert("qos".to_string(), 0.05);

        Self {
            component_weights: weights,
            optimal_dispatcher_workers: num_cpus::get(),
            optimal_crypto_workers: num_cpus::get() * 2,
            optimal_buffer_pool_size: 5000,
            target_latency: Duration::from_millis(50),
            max_throughput: 100000.0,
            resource_utilization: 0.7,
        }
    }

    /// Расчёт оптимального количества воркеров на основе нагрузки
    pub fn calculate_optimal_workers(&self, load_factor: f64, cpu_count: usize) -> usize {
        let base_workers = cpu_count;
        let workers = (base_workers as f64 * (1.0 + load_factor)).round() as usize;
        workers.max(2).min(cpu_count * 4)
    }

    /// Расчёт оптимального размера буферного пула
    pub fn calculate_optimal_buffer_pool(&self, expected_concurrency: usize) -> usize {
        (expected_concurrency as f64 * 1.5).round() as usize
    }
}

impl Default for FactoryOptimizationModel {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct FactoryMetrics {
    pub dispatcher_created: u64,
    pub buffer_pool_created: u64,
    pub crypto_processor_created: u64,
    pub adaptive_batcher_created: u64,
    pub qos_manager_created: u64,
    pub circuit_breaker_created: u64,
    pub total_allocation_time_ms: f64,
    pub average_allocation_time_ms: f64,
    pub memory_allocated_mb: f64,
    pub creation_timestamp: Instant,
}

impl Default for FactoryMetrics {
    fn default() -> Self {
        Self {
            dispatcher_created: 0,
            buffer_pool_created: 0,
            crypto_processor_created: 0,
            adaptive_batcher_created: 0,
            qos_manager_created: 0,
            circuit_breaker_created: 0,
            total_allocation_time_ms: 0.0,
            average_allocation_time_ms: 0.0,
            memory_allocated_mb: 0.0,
            creation_timestamp: Instant::now(),
        }
    }
}

pub struct OptimizedFactory {
    optimization_model: Arc<RwLock<FactoryOptimizationModel>>,
    metrics: Arc<RwLock<FactoryMetrics>>,
    component_cache: Arc<DashMap<String, Arc<dyn std::any::Any + Send + Sync>>>,
    enable_caching: bool,
    enable_optimization: bool,
    enable_metrics: bool,
}

impl OptimizedFactory {
    pub fn new() -> Self {
        info!("🏭 Initializing Mathematical OptimizedFactory v2.0");

        let cpu_count = num_cpus::get();
        info!("  CPU cores: {}", cpu_count);
        info!("  Optimal dispatcher workers: {}", cpu_count);
        info!("  Optimal crypto workers: {}", cpu_count * 2);
        info!("  Caching: enabled");
        info!("  Optimization: enabled");

        Self {
            optimization_model: Arc::new(RwLock::new(FactoryOptimizationModel::new())),
            metrics: Arc::new(RwLock::new(FactoryMetrics::default())),
            component_cache: Arc::new(DashMap::new()),
            enable_caching: true,
            enable_optimization: true,
            enable_metrics: true,
        }
    }

    pub fn create_dispatcher(
        &self,
        num_workers: usize,
        queue_capacity: usize,
        session_manager: Arc<PhantomSessionManager>,
        adaptive_batcher: Arc<AdaptiveBatcher>,
        qos_manager: Arc<QosManager>,
        circuit_breaker: Arc<CircuitBreaker>,
    ) -> Arc<WorkStealingDispatcher> {
        let start_time = Instant::now();

        info!("🚦 Creating WorkStealingDispatcher with {} workers", num_workers);

        let dispatcher = Arc::new(WorkStealingDispatcher::new(
            num_workers,
            queue_capacity,
            session_manager,
            adaptive_batcher,
            qos_manager,
            circuit_breaker,
        ));

        // ИСПРАВЛЕНО: ЯВНО вызываем старт воркеров
        dispatcher.start_workers();
        dispatcher.start_stealing_optimizer();
        dispatcher.start_load_monitor();
        dispatcher.start_metrics_collector();
        dispatcher.start_task_cleaner();

        info!("✅ WorkStealingDispatcher created and STARTED in {:?}", start_time.elapsed());
        info!("   - {} workers ACTIVE", num_workers);
        info!("   - Work stealing: ENABLED");
        info!("   - Load balancing: ACTIVE");

        dispatcher
    }

    pub fn create_buffer_pool(
        &self,
        read_buffer_size: usize,
        write_buffer_size: usize,
        crypto_buffer_size: usize,
        max_buffers: usize,
    ) -> Arc<OptimizedBufferPool> {
        let start_time = Instant::now();

        // Оптимизация размера пула
        let buffer_pool_size = if self.enable_optimization {
            let model_guard = self.optimization_model.try_read()
                .unwrap_or_else(|_| self.optimization_model.blocking_read());
            let model = model_guard.deref();

            let expected_concurrency = max_buffers;
            model.calculate_optimal_buffer_pool(expected_concurrency)
        } else {
            max_buffers
        };

        info!("📦 Creating OptimizedBufferPool with {} buffers", buffer_pool_size);
        info!("  Read buffer: {} KB", read_buffer_size / 1024);
        info!("  Write buffer: {} KB", write_buffer_size / 1024);
        info!("  Crypto buffer: {} KB", crypto_buffer_size / 1024);

        let buffer_pool = Arc::new(OptimizedBufferPool::new(
            read_buffer_size,
            write_buffer_size,
            crypto_buffer_size,
            buffer_pool_size,
        ));

        // Кэширование
        if self.enable_caching {
            self.component_cache.insert(
                "buffer_pool".to_string(),
                buffer_pool.clone() as Arc<dyn std::any::Any + Send + Sync>
            );
        }

        // Обновление метрик
        if self.enable_metrics {
            if let Ok(mut metrics) = self.metrics.try_write() {
                metrics.buffer_pool_created += 1;
                metrics.total_allocation_time_ms += start_time.elapsed().as_millis() as f64;
                metrics.memory_allocated_mb += (read_buffer_size + write_buffer_size + crypto_buffer_size) as f64
                    * buffer_pool_size as f64 / 1024.0 / 1024.0;
            }
        }

        info!("✅ OptimizedBufferPool created in {:?}", start_time.elapsed());

        buffer_pool
    }

    pub fn create_crypto_processor(&self, num_workers: usize) -> Arc<OptimizedCryptoProcessor> {
        let start_time = Instant::now();

        // Оптимизация количества воркеров
        let workers = if self.enable_optimization {
            let model_guard = self.optimization_model.try_read()
                .unwrap_or_else(|_| self.optimization_model.blocking_read());
            let model = model_guard.deref();

            let cpu_count = num_cpus::get();
            let load_factor = 0.8; // Криптооперации более требовательны
            model.calculate_optimal_workers(load_factor, cpu_count)
                .max(num_workers)
        } else {
            num_workers
        };

        info!("🔐 Creating OptimizedCryptoProcessor with {} workers", workers);
        info!("  SIMD: ChaCha20 + Blake3");
        info!("  Batch processing: enabled");

        let crypto_processor = Arc::new(OptimizedCryptoProcessor::new(workers));

        // Кэширование
        if self.enable_caching {
            self.component_cache.insert(
                "crypto_processor".to_string(),
                crypto_processor.clone() as Arc<dyn std::any::Any + Send + Sync>
            );
        }

        // Обновление метрик
        if self.enable_metrics {
            if let Ok(mut metrics) = self.metrics.try_write() {
                metrics.crypto_processor_created += 1;
                metrics.total_allocation_time_ms += start_time.elapsed().as_millis() as f64;
            }
        }

        info!("✅ OptimizedCryptoProcessor created in {:?}", start_time.elapsed());

        crypto_processor
    }
}

impl Default for OptimizedFactory {
    fn default() -> Self {
        Self::new()
    }
}

use dashmap::DashMap;
use std::ops::Deref;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{info};