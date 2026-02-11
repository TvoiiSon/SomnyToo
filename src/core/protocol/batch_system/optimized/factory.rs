use std::sync::Arc;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use super::work_stealing_dispatcher::WorkStealingDispatcher;
use super::buffer_pool::OptimizedBufferPool;
use super::crypto_processor::OptimizedCryptoProcessor;

// ИМПОРТЫ ДЛЯ ИНТЕГРАЦИИ
use crate::core::protocol::batch_system::adaptive_batcher::AdaptiveBatcher;
use crate::core::protocol::batch_system::qos_manager::QosManager;
use crate::core::protocol::batch_system::circuit_breaker::CircuitBreaker;

/// Фабрика для создания оптимизированных компонентов
pub struct OptimizedFactory;

impl OptimizedFactory {
    /// Создание оптимизированного work-stealing диспетчера с полной интеграцией
    pub fn create_dispatcher(
        num_workers: usize,
        queue_capacity: usize,
        session_manager: Arc<PhantomSessionManager>,
        adaptive_batcher: Arc<AdaptiveBatcher>,
        qos_manager: Arc<QosManager>,
        circuit_breaker: Arc<CircuitBreaker>,
    ) -> Arc<WorkStealingDispatcher> {
        Arc::new(WorkStealingDispatcher::new(
            num_workers,
            queue_capacity,
            session_manager,
            adaptive_batcher,
            qos_manager,
            circuit_breaker,
        ))
    }

    /// Создание оптимизированного пула буферов
    pub fn create_buffer_pool(
        read_buffer_size: usize,
        write_buffer_size: usize,
        crypto_buffer_size: usize,
        max_buffers: usize,
    ) -> Arc<OptimizedBufferPool> {
        Arc::new(OptimizedBufferPool::new(
            read_buffer_size,
            write_buffer_size,
            crypto_buffer_size,
            max_buffers,
        ))
    }

    /// Создание оптимизированного криптопроцессора с SIMD акселерацией
    pub fn create_crypto_processor(num_workers: usize) -> Arc<OptimizedCryptoProcessor> {
        Arc::new(OptimizedCryptoProcessor::new(num_workers))
    }
}