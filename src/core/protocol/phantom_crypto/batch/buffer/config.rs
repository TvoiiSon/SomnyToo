use std::collections::HashMap;
use std::time::Duration;

use super::buffer_types::BufferType;

/// Конфигурация пула буферов
#[derive(Debug, Clone)]
pub struct BufferPoolConfig {
    pub initial_capacity: HashMap<BufferType, usize>,
    pub max_capacity: HashMap<BufferType, usize>,
    pub buffer_sizes: HashMap<BufferType, usize>,
    pub shrink_interval: Duration,
    pub enable_monitoring: bool,
    pub high_memory_threshold: f64, // % от max_capacity
}

impl Default for BufferPoolConfig {
    fn default() -> Self {
        let mut initial_capacity = HashMap::new();
        let mut max_capacity = HashMap::new();
        let mut buffer_sizes = HashMap::new();

        // Настройки для каждого типа буфера
        let types_and_sizes = [
            (BufferType::Encryption, 65536, 1000, 5000),
            (BufferType::Decryption, 65536, 1000, 5000),
            (BufferType::NetworkRead, 8192, 2000, 10000),
            (BufferType::NetworkWrite, 8192, 2000, 10000),
            (BufferType::Header, 256, 5000, 20000),
            (BufferType::CryptoKey, 64, 1000, 5000),
            (BufferType::BatchStorage, 131072, 100, 500),
        ];

        for (buffer_type, size, initial, max) in types_and_sizes {
            initial_capacity.insert(buffer_type, initial);
            max_capacity.insert(buffer_type, max);
            buffer_sizes.insert(buffer_type, size);
        }

        Self {
            initial_capacity,
            max_capacity,
            buffer_sizes,
            shrink_interval: Duration::from_secs(60),
            enable_monitoring: true,
            high_memory_threshold: 0.8,
        }
    }
}