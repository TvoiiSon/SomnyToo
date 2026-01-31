use std::sync::Arc;
use std::collections::HashMap;
use std::time::{Instant, Duration};
use parking_lot::{RwLock, Mutex};
use tracing::{debug, info, warn};
use bytes::{Bytes, BytesMut};

/// Типы буферов в системе
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufferType {
    Encryption,     // Для шифрования (большие)
    Decryption,     // Для дешифрования (средние)
    NetworkRead,    // Чтение из сети (малые)
    NetworkWrite,   // Запись в сеть (малые)
    Header,         // Заголовки пакетов (фиксированные)
    CryptoKey,      // Ключевой материал
    BatchStorage,   // Хранение батчей
}

/// Статистика использования буферов
#[derive(Debug, Default, Clone)]
pub struct BufferStats {
    pub total_allocated: usize,
    pub currently_used: usize,
    pub allocation_count: u64,
    pub reuse_count: u64,
    pub hit_rate: f64,
    pub avg_buffer_size: f64,
    pub memory_pressure: f64, // 0.0 - 1.0
}

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

/// Буфер с метаданными
struct PooledBuffer {
    buffer: BytesMut,
    buffer_type: BufferType,
    created_at: Instant,
    last_used: Instant,
    size: usize,
    is_used: bool,
}

/// Единый пул буферов для всей системы
pub struct UnifiedBufferPool {
    config: BufferPoolConfig,
    pools: RwLock<HashMap<BufferType, Vec<PooledBuffer>>>,
    stats: RwLock<HashMap<BufferType, BufferStats>>,
    global_stats: Mutex<GlobalBufferStats>,
    shrink_timer: Mutex<Option<tokio::time::Interval>>,
}

#[derive(Debug, Clone)]
pub struct GlobalBufferStats {
    total_memory_allocated: usize,
    peak_memory_usage: usize,
    total_allocations: u64,
    total_reuses: u64,
    memory_pressure_alerts: u64,
    last_shrink_time: Instant,
}

impl Default for GlobalBufferStats {
    fn default() -> Self {
        Self {
            total_memory_allocated: 0,
            peak_memory_usage: 0,
            total_allocations: 0,
            total_reuses: 0,
            memory_pressure_alerts: 0,
            last_shrink_time: Instant::now(),
        }
    }
}

impl UnifiedBufferPool {
    /// Создание нового пула буферов
    pub fn new(config: BufferPoolConfig) -> Self {
        let mut pools = HashMap::new();
        let mut stats = HashMap::new();

        // Инициализируем пулы для каждого типа
        for (&buffer_type, &initial_capacity) in &config.initial_capacity {
            let mut buffer_pool = Vec::with_capacity(initial_capacity);
            let buffer_size = config.buffer_sizes[&buffer_type];

            for _ in 0..initial_capacity {
                buffer_pool.push(PooledBuffer {
                    buffer: BytesMut::with_capacity(buffer_size),
                    buffer_type,
                    created_at: Instant::now(),
                    last_used: Instant::now(),
                    size: buffer_size,
                    is_used: false,
                });
            }

            pools.insert(buffer_type, buffer_pool);
            stats.insert(buffer_type, BufferStats::default());
        }

        let pool = Self {
            config: config.clone(),
            pools: RwLock::new(pools),
            stats: RwLock::new(stats),
            global_stats: Mutex::new(GlobalBufferStats::default()),
            shrink_timer: Mutex::new(None),
        };

        // Запускаем мониторинг если включен
        if config.enable_monitoring {
            pool.start_monitoring();
        }

        info!("🔄 UnifiedBufferPool initialized with {} buffer types",
              config.initial_capacity.len());

        pool
    }

    /// Получение буфера из пула
    pub fn acquire(&self, buffer_type: BufferType, min_size: usize) -> Option<BufferHandle> {
        let _start = Instant::now();
        let mut pools = self.pools.write();
        let mut stats = self.stats.write();

        let buffer_stats = stats.entry(buffer_type).or_insert_with(BufferStats::default);
        let pool = pools.entry(buffer_type).or_insert_with(Vec::new);

        // Ищем свободный буфер подходящего размера
        for i in 0..pool.len() {
            if !pool[i].is_used && pool[i].buffer.capacity() >= min_size {
                let mut buffer = pool.swap_remove(i);
                buffer.is_used = true;
                buffer.last_used = Instant::now();

                // Обновляем статистику
                buffer_stats.currently_used += 1;
                buffer_stats.reuse_count += 1;

                {
                    let mut global_stats = self.global_stats.lock();
                    global_stats.total_reuses += 1;
                }

                debug!("Buffer acquired: {:?}, size: {}, reuse",
                       buffer_type, buffer.buffer.capacity());

                return Some(BufferHandle {
                    buffer: buffer.buffer,
                    buffer_type,
                    pool: Arc::new(self.clone()),
                });
            }
        }

        // Не нашли подходящий буфер - создаем новый
        let buffer_size = self.config.buffer_sizes
            .get(&buffer_type)
            .copied()
            .unwrap_or(8192)
            .max(min_size);

        // Проверяем лимиты памяти
        if self.check_memory_pressure(buffer_type, buffer_size) {
            warn!("Memory pressure high for {:?}, falling back to direct allocation",
                  buffer_type);
            return None;
        }

        let new_buffer = PooledBuffer {
            buffer: BytesMut::with_capacity(buffer_size),
            buffer_type,
            created_at: Instant::now(),
            last_used: Instant::now(),
            size: buffer_size,
            is_used: true,
        };

        // Обновляем статистику
        buffer_stats.total_allocated += buffer_size;
        buffer_stats.currently_used += 1;
        buffer_stats.allocation_count += 1;

        {
            let mut global_stats = self.global_stats.lock();
            global_stats.total_memory_allocated += buffer_size;
            global_stats.total_allocations += 1;
            global_stats.peak_memory_usage = global_stats.peak_memory_usage
                .max(global_stats.total_memory_allocated);
        }

        debug!("Buffer allocated: {:?}, size: {}, new allocation",
               buffer_type, buffer_size);

        Some(BufferHandle {
            buffer: new_buffer.buffer,
            buffer_type,
            pool: Arc::new(self.clone()),
        })
    }

    /// Возврат буфера в пул
    fn release(&self, mut buffer: BytesMut, buffer_type: BufferType) {
        let mut pools = self.pools.write();
        let mut stats = self.stats.write();

        let buffer_stats = stats.entry(buffer_type).or_insert_with(BufferStats::default);
        let pool = pools.entry(buffer_type).or_insert_with(Vec::new);

        // Проверяем, не превышаем ли максимальную емкость
        let max_capacity = self.config.max_capacity.get(&buffer_type).copied().unwrap_or(1000);
        if pool.len() >= max_capacity {
            // Освобождаем самый старый неиспользуемый буфер
            if let Some(oldest_idx) = pool.iter()
                .enumerate()
                .filter(|(_, b)| !b.is_used)
                .min_by_key(|(_, b)| b.last_used)
                .map(|(idx, _)| idx) {
                pool.swap_remove(oldest_idx);
            }
        }

        // Очищаем буфер перед возвращением
        buffer.clear();

        let size = buffer.capacity();

        // Сохраняем буфер в пул
        pool.push(PooledBuffer {
            buffer,
            buffer_type,
            created_at: Instant::now(),
            last_used: Instant::now(),
            size,
            is_used: false,
        });

        // Обновляем статистику
        buffer_stats.currently_used = buffer_stats.currently_used.saturating_sub(1);

        // Обновляем hit rate
        let total_accesses = buffer_stats.allocation_count + buffer_stats.reuse_count;
        if total_accesses > 0 {
            buffer_stats.hit_rate = buffer_stats.reuse_count as f64 / total_accesses as f64;
        }

        // Обновляем средний размер
        let total_buffers = buffer_stats.allocation_count + buffer_stats.reuse_count;
        buffer_stats.avg_buffer_size = buffer_stats.total_allocated as f64 /
            total_buffers.max(1) as f64;
    }

    /// Проверка давления памяти
    fn check_memory_pressure(&self, buffer_type: BufferType, requested_size: usize) -> bool {
        let _pools = self.pools.read();
        let stats = self.stats.read();

        let buffer_stats = match stats.get(&buffer_type) {
            Some(stats) => stats,
            None => return false,
        };

        let max_capacity = self.config.max_capacity.get(&buffer_type).copied().unwrap_or(1000);

        // Рассчитываем текущее использование
        let current_usage = buffer_stats.currently_used as f64 / max_capacity as f64;

        // Учитываем запрашиваемый размер
        let buffer_size = self.config.buffer_sizes.get(&buffer_type).copied().unwrap_or(8192);
        let size_pressure = requested_size as f64 / buffer_size as f64;

        let total_pressure = current_usage.max(size_pressure);

        // Проверяем порог
        total_pressure > self.config.high_memory_threshold
    }

    /// Запуск мониторинга
    fn start_monitoring(&self) {
        let pool = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));

            loop {
                interval.tick().await;
                pool.log_stats();
                pool.auto_shrink();
            }
        });
    }

    /// Автоматическое сжатие пулов
    fn auto_shrink(&self) {
        let mut global_stats = self.global_stats.lock();
        let now = Instant::now();

        if now.duration_since(global_stats.last_shrink_time) < self.config.shrink_interval {
            return;
        }

        global_stats.last_shrink_time = now;

        let mut pools = self.pools.write();
        let mut stats = self.stats.write();

        for (buffer_type, pool) in pools.iter_mut() {
            let buffer_stats = stats.entry(*buffer_type).or_insert_with(BufferStats::default);

            // Создаем новый список с только используемыми или недавно использованными буферами
            let five_minutes_ago = now - Duration::from_secs(300);
            let mut new_pool = Vec::new();

            for buffer in pool.drain(..) {
                if buffer.is_used || buffer.last_used > five_minutes_ago {
                    new_pool.push(buffer);
                }
            }

            *pool = new_pool;

            // Обновляем статистику
            buffer_stats.currently_used = pool.iter().filter(|b| b.is_used).count();

            // Проверяем hit rate, если низкий - уменьшаем пул
            if buffer_stats.hit_rate < 0.3 && pool.len() > 10 {
                let to_remove = pool.len() / 2; // Удаляем половину
                pool.truncate(pool.len() - to_remove);
                debug!("Shrunk {:?} pool: removed {} buffers", buffer_type, to_remove);
            }
        }
    }

    /// Логирование статистики
    fn log_stats(&self) {
        let pools = self.pools.read();
        let stats = self.stats.read();
        let global_stats = self.global_stats.lock();

        info!("📊 Buffer Pool Statistics:");
        info!("  Total memory: {:.2} MB",
              global_stats.total_memory_allocated as f64 / 1024.0 / 1024.0);
        info!("  Peak memory: {:.2} MB",
              global_stats.peak_memory_usage as f64 / 1024.0 / 1024.0);
        info!("  Total allocations: {}", global_stats.total_allocations);
        info!("  Total reuses: {}", global_stats.total_reuses);

        for (buffer_type, buffer_stats) in stats.iter() {
            let pool_size = pools.get(buffer_type).map(|p| p.len()).unwrap_or(0);
            info!("  {:?}: pool={}, used={}, hit_rate={:.1}%, pressure={:.1}%",
                  buffer_type,
                  pool_size,
                  buffer_stats.currently_used,
                  buffer_stats.hit_rate * 100.0,
                  buffer_stats.memory_pressure * 100.0);
        }
    }

    /// Принудительное освобождение всех буферов
    pub fn force_cleanup(&self) {
        let mut pools = self.pools.write();
        let mut stats = self.stats.write();
        let mut global_stats = self.global_stats.lock();

        for (buffer_type, pool) in pools.iter_mut() {
            // Создаем новый список только с используемыми буферами
            let mut used_buffers = Vec::new();
            for buffer in pool.drain(..) {
                if buffer.is_used {
                    used_buffers.push(buffer);
                }
            }
            *pool = used_buffers;

            // Обновляем статистику
            if let Some(buffer_stats) = stats.get_mut(buffer_type) {
                buffer_stats.currently_used = pool.len();
            }
        }

        global_stats.total_memory_allocated = stats.values()
            .map(|s| s.total_allocated)
            .sum();

        info!("Forced buffer pool cleanup completed");
    }

    /// Получение статистики по типу буфера
    pub fn get_stats(&self, buffer_type: BufferType) -> Option<BufferStats> {
        self.stats.read().get(&buffer_type).cloned()
    }

    /// Получение глобальной статистики
    pub fn get_global_stats(&self) -> GlobalBufferStats {
        self.global_stats.lock().clone()
    }
}

/// Хендл для буфера, автоматически возвращающий его в пул при drop
pub struct BufferHandle {
    buffer: BytesMut,
    buffer_type: BufferType,
    pool: Arc<UnifiedBufferPool>,
}

impl BufferHandle {
    /// Получение сырого буфера
    pub fn buffer_mut(&mut self) -> &mut BytesMut {
        &mut self.buffer
    }

    /// Получение буфера как slice
    pub fn as_slice(&self) -> &[u8] {
        &self.buffer
    }

    /// Получение буфера как mutable slice
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    /// Размер буфера
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Емкость буфера
    pub fn capacity(&self) -> usize {
        self.buffer.capacity()
    }

    /// Тип буфера
    pub fn buffer_type(&self) -> BufferType {
        self.buffer_type
    }

    /// Преобразование в Bytes (перенос владения)
    pub fn freeze(self) -> Bytes {
        self.buffer.clone().freeze()
    }
}

impl Drop for BufferHandle {
    fn drop(&mut self) {
        let buffer = std::mem::take(&mut self.buffer);
        self.pool.release(buffer, self.buffer_type);
    }
}

impl Clone for UnifiedBufferPool {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            pools: RwLock::new(HashMap::new()), // Новые пулы для каждого экземпляра
            stats: RwLock::new(HashMap::new()),
            global_stats: Mutex::new(GlobalBufferStats::default()),
            shrink_timer: Mutex::new(None),
        }
    }
}