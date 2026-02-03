use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::HashMap;
use bytes::BytesMut;
use tracing::{info, debug, warn};
use parking_lot::{RwLock, Mutex};

use crate::core::protocol::phantom_crypto::batch::config::BatchConfig;

/// Типы буферов
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufferType {
    Read,        // Для чтения из сети
    Write,       // Для записи в сеть
    Crypto,      // Для криптографических операций
    Temporary,   // Временные буферы
}

impl BufferType {
    pub fn default_size(&self) -> usize {
        match self {
            BufferType::Read => 8192,      // 8KB для чтения
            BufferType::Write => 8192,     // 8KB для записи
            BufferType::Crypto => 65536,   // 64KB для криптографии
            BufferType::Temporary => 32768, // 32KB для временных данных
        }
    }

    pub fn max_size(&self) -> usize {
        match self {
            BufferType::Read => 131072,    // 128KB максимально
            BufferType::Write => 131072,   // 128KB максимально
            BufferType::Crypto => 262144,  // 256KB максимально
            BufferType::Temporary => 65536, // 64KB максимально
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            BufferType::Read => "Read",
            BufferType::Write => "Write",
            BufferType::Crypto => "Crypto",
            BufferType::Temporary => "Temporary",
        }
    }
}

/// Хендл для буфера с автоматическим освобождением
pub struct BufferHandle {
    buffer: BytesMut,
    buffer_type: BufferType,
    pool: Arc<UnifiedBufferPool>,
}

impl BufferHandle {
    pub fn buffer_mut(&mut self) -> &mut BytesMut {
        &mut self.buffer
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buffer
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.buffer.capacity()
    }

    pub fn freeze(mut self) -> bytes::Bytes {
        std::mem::take(&mut self.buffer).freeze()
    }

    pub fn extend_from_slice(&mut self, slice: &[u8]) {
        self.buffer.extend_from_slice(slice);
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    pub fn buffer_type(&self) -> BufferType {
        self.buffer_type
    }

    pub fn buffer_type_name(&self) -> &'static str {
        self.buffer_type.name()
    }
}

impl Drop for BufferHandle {
    fn drop(&mut self) {
        let buffer = std::mem::take(&mut self.buffer);
        self.pool.release_buffer(buffer, self.buffer_type);
    }
}

/// Буфер в пуле
struct PooledBuffer {
    buffer: BytesMut,
    buffer_type: BufferType,
    created_at: Instant,
    last_used: Instant,
    size: usize,
    is_used: bool,
}

impl PooledBuffer {
    fn can_reuse_for(&self, buffer_type: BufferType, min_size: usize) -> bool {
        !self.is_used &&
            self.buffer_type == buffer_type &&
            self.buffer.capacity() >= min_size
    }
}

/// Единый пул буферов
pub struct UnifiedBufferPool {
    config: BatchConfig,
    pools: RwLock<HashMap<BufferType, Vec<PooledBuffer>>>,
    stats: Mutex<BufferStats>,
}

/// Статистика буферного пула
#[derive(Debug, Clone)]
pub struct BufferStats {
    pub total_allocated: usize,
    pub currently_used: usize,
    pub allocation_count: u64,
    pub reuse_count: u64,
    pub memory_pressure_alerts: u64,
    pub peak_memory_usage: usize,
}

impl Default for BufferStats {
    fn default() -> Self {
        Self {
            total_allocated: 0,
            currently_used: 0,
            allocation_count: 0,
            reuse_count: 0,
            memory_pressure_alerts: 0,
            peak_memory_usage: 0,
        }
    }
}

impl UnifiedBufferPool {
    pub fn new(config: BatchConfig) -> Self {
        let mut pools = HashMap::new();

        // Инициализируем пулы для каждого типа буферов
        for &buffer_type in &[BufferType::Read, BufferType::Write, BufferType::Crypto, BufferType::Temporary] {
            pools.insert(buffer_type, Vec::with_capacity(32));
        }

        Self {
            config,
            pools: RwLock::new(pools),
            stats: Mutex::new(BufferStats::default()),
        }
    }

    /// Получение буфера из пула
    pub fn acquire_buffer(&self, buffer_type: BufferType, min_size: usize) -> BufferHandle {
        let start = Instant::now();
        let mut pools = self.pools.write();
        let mut stats = self.stats.lock();

        let pool = pools.entry(buffer_type).or_insert_with(|| Vec::with_capacity(32));

        // Ищем подходящий свободный буфер
        for i in 0..pool.len() {
            if pool[i].can_reuse_for(buffer_type, min_size) {
                let mut buffer = pool.swap_remove(i);
                buffer.is_used = true;
                buffer.last_used = Instant::now();

                stats.currently_used += 1;
                stats.reuse_count += 1;

                debug!("Buffer acquired: {:?}, size: {}, reuse, time: {:?}",
                       buffer_type, buffer.buffer.capacity(), start.elapsed());

                return BufferHandle {
                    buffer: buffer.buffer,
                    buffer_type,
                    pool: Arc::new(self.clone()),
                };
            }
        }

        // Не нашли подходящий буфер - создаем новый
        let buffer_size = buffer_type.default_size().max(min_size).min(buffer_type.max_size());

        // Проверяем давление памяти
        if self.check_memory_pressure(&stats) {
            warn!("Memory pressure high, allocating minimal buffer");
        }

        let buffer = BytesMut::with_capacity(buffer_size);

        // Обновляем статистику
        stats.total_allocated += buffer_size;
        stats.currently_used += 1;
        stats.allocation_count += 1;
        let peak = stats.peak_memory_usage;
        stats.peak_memory_usage = peak.max(stats.total_allocated);

        debug!("Buffer allocated: {:?}, size: {}, new allocation, time: {:?}",
               buffer_type, buffer_size, start.elapsed());

        BufferHandle {
            buffer,
            buffer_type,
            pool: Arc::new(self.clone()),
        }
    }

    /// Освобождение буфера (внутренний метод)
    fn release_buffer(&self, mut buffer: BytesMut, buffer_type: BufferType) {
        let mut pools = self.pools.write();
        let mut stats = self.stats.lock();

        // Очищаем буфер перед возвращением
        buffer.clear();

        let pool = pools.entry(buffer_type).or_insert_with(|| Vec::with_capacity(32));

        // Проверяем максимальный размер пула
        let max_pool_size = 100; // Максимум 100 буферов на тип
        if pool.len() >= max_pool_size {
            // Удаляем самый старый неиспользуемый буфер
            if let Some(oldest_idx) = pool.iter()
                .enumerate()
                .filter(|(_, b)| !b.is_used)
                .min_by_key(|(_, b)| b.last_used)
                .map(|(idx, _)| idx) {
                pool.swap_remove(oldest_idx);
            }
        }

        let size = buffer.capacity();

        pool.push(PooledBuffer {
            buffer,
            buffer_type,
            created_at: Instant::now(),
            last_used: Instant::now(),
            size,
            is_used: false,
        });

        // Обновляем статистику
        stats.currently_used = stats.currently_used.saturating_sub(1);
    }

    /// Проверка давления памяти
    fn check_memory_pressure(&self, stats: &BufferStats) -> bool {
        let max_memory = 1024 * 1024 * 512; // 512MB максимум
        let current_usage = stats.total_allocated as f64 / max_memory as f64;

        if current_usage > 0.8 {
            // Высокое давление памяти
            let mut stats = self.stats.lock();
            stats.memory_pressure_alerts += 1;
            true
        } else {
            false
        }
    }

    /// Очистка старых буферов
    pub fn cleanup_old_buffers(&self, max_age: Duration) {
        let mut pools = self.pools.write();
        let now = Instant::now();
        let mut cleaned = 0;

        for (buffer_type, pool) in pools.iter_mut() {
            let before = pool.len();

            pool.retain(|b| {
                if !b.is_used && now.duration_since(b.created_at) > max_age {
                    debug!("Cleaning up old buffer: {:?}, age: {:?}",
                           buffer_type, now.duration_since(b.created_at));
                    false
                } else {
                    true
                }
            });

            cleaned += before - pool.len();
        }

        if cleaned > 0 {
            debug!("Cleaned up {} old buffers", cleaned);
        }
    }

    /// Принудительная очистка всех неиспользуемых буферов
    pub fn force_cleanup(&self) {
        let mut pools = self.pools.write();
        let mut stats = self.stats.lock();

        for pool in pools.values_mut() {
            pool.retain(|b| b.is_used);
        }

        // Пересчитываем статистику
        stats.total_allocated = pools.values()
            .flat_map(|p| p.iter())
            .map(|b| b.size)
            .sum();
        stats.currently_used = pools.values()
            .flat_map(|p| p.iter())
            .filter(|b| b.is_used)
            .count();
    }

    /// Получение статистики
    pub fn get_stats(&self) -> BufferStats {
        self.stats.lock().clone()
    }

    /// Логирование статистики
    pub fn log_stats(&self) {
        let stats = self.get_stats();
        let pools = self.pools.read();

        info!("📊 Buffer Pool Statistics:");
        info!("  Total allocated: {:.2} MB", stats.total_allocated as f64 / 1024.0 / 1024.0);
        info!("  Currently used: {} buffers", stats.currently_used);
        info!("  Allocation count: {}", stats.allocation_count);
        info!("  Reuse count: {}", stats.reuse_count);
        info!("  Peak memory: {:.2} MB", stats.peak_memory_usage as f64 / 1024.0 / 1024.0);

        let hit_rate = if stats.allocation_count + stats.reuse_count > 0 {
            stats.reuse_count as f64 / (stats.allocation_count + stats.reuse_count) as f64 * 100.0
        } else {
            0.0
        };

        info!("  Hit rate: {:.1}%", hit_rate);

        for (buffer_type, pool) in pools.iter() {
            let pool_size = pool.len();
            let used_count = pool.iter().filter(|b| b.is_used).count();
            let total_size: usize = pool.iter().map(|b| b.size).sum();

            info!("  {:?}: pool={}, used={}, total_size={:.2} KB",
                  buffer_type, pool_size, used_count, total_size as f64 / 1024.0);
        }
    }

    /// Получение информации о памяти
    pub fn memory_info(&self) -> MemoryInfo {
        let stats = self.get_stats();
        let pools = self.pools.read();

        let mut buffer_type_counts = HashMap::new();
        let mut buffer_type_sizes = HashMap::new();

        for (buffer_type, pool) in pools.iter() {
            let used_count = pool.iter().filter(|b| b.is_used).count();
            let total_size: usize = pool.iter().map(|b| b.size).sum();

            buffer_type_counts.insert(*buffer_type, (used_count, pool.len() - used_count));
            buffer_type_sizes.insert(*buffer_type, total_size);
        }

        MemoryInfo {
            total_allocated: stats.total_allocated,
            currently_used: stats.currently_used,
            allocation_count: stats.allocation_count,
            reuse_count: stats.reuse_count,
            hit_rate: if stats.allocation_count + stats.reuse_count > 0 {
                stats.reuse_count as f64 / (stats.allocation_count + stats.reuse_count) as f64
            } else {
                0.0
            },
            buffer_type_counts,
            buffer_type_sizes,
        }
    }
}

/// Информация о памяти
#[derive(Debug, Clone)]
pub struct MemoryInfo {
    pub total_allocated: usize,
    pub currently_used: usize,
    pub allocation_count: u64,
    pub reuse_count: u64,
    pub hit_rate: f64,
    pub buffer_type_counts: HashMap<BufferType, (usize, usize)>, // (used, free)
    pub buffer_type_sizes: HashMap<BufferType, usize>,
}

impl Clone for UnifiedBufferPool {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            pools: RwLock::new(HashMap::new()),
            stats: Mutex::new(BufferStats::default()),
        }
    }
}