use std::collections::HashMap;
use std::time::Duration;

use crate::core::protocol::phantom_crypto::batch::types::priority::BatchPriority;

/// Конфигурация диспетчера пакетов
#[derive(Debug, Clone)]
pub struct PacketBatchDispatcherConfig {
    pub worker_count: usize,                 // Количество worker-ов
    pub max_queue_size: usize,               // Максимальный размер очереди
    pub batch_timeout: Duration,             // Таймаут обработки батча
    pub enable_work_stealing: bool,          // Включить work stealing
    pub max_steal_attempts: usize,           // Максимальное количество попыток stealing
    pub load_balancing_interval: Duration,   // Интервал балансировки нагрузки
    pub emergency_flush_threshold: usize,    // Порог аварийного сброса
    pub priority_queues: usize,              // Количество приоритетных очередей
    pub batch_size_per_priority: HashMap<BatchPriority, usize>, // Размер батча по приоритетам
}

impl Default for PacketBatchDispatcherConfig {
    fn default() -> Self {
        let mut batch_size_per_priority = HashMap::new();
        batch_size_per_priority.insert(BatchPriority::Realtime, 8);
        batch_size_per_priority.insert(BatchPriority::High, 16);
        batch_size_per_priority.insert(BatchPriority::Normal, 64);
        batch_size_per_priority.insert(BatchPriority::Low, 128);
        batch_size_per_priority.insert(BatchPriority::Background, 256);

        Self {
            worker_count: 4,
            max_queue_size: 10000,
            batch_timeout: Duration::from_secs(5),
            enable_work_stealing: true,
            max_steal_attempts: 3,
            load_balancing_interval: Duration::from_secs(10),
            emergency_flush_threshold: 5000,
            priority_queues: 5,
            batch_size_per_priority,
        }
    }
}