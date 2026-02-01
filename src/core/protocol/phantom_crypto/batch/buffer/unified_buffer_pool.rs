use std::sync::Arc;
use std::collections::HashMap;
use std::time::{Instant, Duration};
use parking_lot::{RwLock, Mutex};
use tracing::{debug, info, warn};
use bytes::{BytesMut};

pub(crate) use super::config::BufferPoolConfig;
use super::buffer_types::BufferType;
use super::buffer_stats::{BufferStats, GlobalBufferStats};
use super::buffer_handle::BufferHandle;

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
        let start = Instant::now();
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

                debug!("Buffer acquired: {:?}, size: {}, reuse, time: {:?}",
                       buffer_type, buffer.buffer.capacity(), start.elapsed());

                // Используем конструктор вместо прямого создания структуры
                return Some(BufferHandle::new(buffer.buffer, buffer_type, Arc::new(self.clone())));
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

        debug!("Buffer allocated: {:?}, size: {}, new allocation, time: {:?}",
               buffer_type, buffer_size, start.elapsed());

        // Используем конструктор
        Some(BufferHandle::new(new_buffer.buffer, buffer_type, Arc::new(self.clone())))
    }

    /// Возврат буфера в пул (внутренний метод)
    pub(crate) fn release(&self, mut buffer: BytesMut, buffer_type: BufferType) {
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

    pub fn log_pool_stats(&self) {
        self.log_stats();
    }

    pub fn cleanup_old_buffers(&self, max_age: Duration) {
        let mut pools = self.pools.write();

        for (buffer_type, pool) in pools.iter_mut() {
            self.cleanup_old_buffers_internal(pool, max_age);
            info!("Cleaned {:?} buffer pool", buffer_type);
        }
    }

    /// Проверка давления памяти
    fn check_memory_pressure(&self, buffer_type: BufferType, requested_size: usize) -> bool {
        let _pools = self.pools.read(); // Добавляем префикс _
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
        if total_pressure > self.config.high_memory_threshold {
            let mut global_stats = self.global_stats.lock();
            global_stats.memory_pressure_alerts += 1;
            return true;
        }

        false
    }

    /// Запуск мониторинга
    fn start_monitoring(&self) {
        let pool = self.clone();
        let shrink_interval = self.config.shrink_interval;

        // Запускаем асинхронную задачу для мониторинга
        tokio::spawn(async move {
            let mut monitoring_interval = tokio::time::interval(Duration::from_secs(10));
            let mut shrink_interval_timer = tokio::time::interval(shrink_interval);

            monitoring_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            shrink_interval_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            // Сохраняем клон таймера в структуре
            {
                let mut shrink_timer = pool.shrink_timer.lock();
                *shrink_timer = Some(tokio::time::interval(shrink_interval));
            }

            loop {
                tokio::select! {
                // Мониторинг каждые 10 секунд
                _ = monitoring_interval.tick() => {
                    pool.log_stats();
                }
                // Автоматическое сжатие по отдельному таймеру
                _ = shrink_interval_timer.tick() => {
                    pool.auto_shrink();
                }
            }
            }
        });
    }

    /// Автоматическое сжатие пулов
    fn auto_shrink(&self) {
        let mut global_stats = self.global_stats.lock();
        let now = Instant::now();

        // Проверяем, прошло ли достаточно времени с последнего сжатия
        if now.duration_since(global_stats.last_shrink_time) < self.config.shrink_interval {
            return;
        }

        global_stats.last_shrink_time = now;
        drop(global_stats); // Освобождаем блокировку раньше

        let mut pools = self.pools.write();
        let mut stats = self.stats.write();

        for (buffer_type, pool) in pools.iter_mut() {
            let buffer_stats = stats.entry(*buffer_type).or_insert_with(BufferStats::default);

            // ВЫЗЫВАЕМ метод cleanup_old_buffers
            self.cleanup_old_buffers_internal(pool, Duration::from_secs(300)); // 5 минут

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

    /// Запуск немедленного сжатия
    pub async fn force_shrink(&self) {
        info!("🔄 Forcing immediate buffer pool shrink");
        self.auto_shrink();
    }

    /// Получение информации о следующем сжатии
    pub fn next_shrink_in(&self) -> Option<Duration> {
        let global_stats = self.global_stats.lock();
        let now = Instant::now();

        if global_stats.last_shrink_time == Instant::now() {
            // Еще не было сжатия
            return Some(self.config.shrink_interval);
        }

        let time_since_last_shrink = now.duration_since(global_stats.last_shrink_time);
        if time_since_last_shrink < self.config.shrink_interval {
            Some(self.config.shrink_interval - time_since_last_shrink)
        } else {
            Some(Duration::from_secs(0))
        }
    }

    /// Остановка мониторинга и очистка ресурсов
    pub async fn shutdown(&self) {
        info!("Shutting down UnifiedBufferPool");

        // Очищаем таймер
        {
            let mut shrink_timer = self.shrink_timer.lock();
            *shrink_timer = None;
        }

        // Принудительное сжатие перед выключением
        self.force_cleanup();

        info!("UnifiedBufferPool shutdown complete");
    }

    fn cleanup_old_buffers_internal(&self, pool: &mut Vec<PooledBuffer>, max_age: Duration) {
        let now = Instant::now();
        let before = pool.len();

        pool.retain(|b| {
            // Используем поля created_at и buffer_type
            let age = now.duration_since(b.created_at);
            let is_old = age > max_age;

            if !b.is_used && is_old {
                debug!("Removing old {:?} buffer: size={} bytes, age={:?}",
                       b.buffer_type, b.size, age);
                false
            } else {
                true
            }
        });

        let removed = before - pool.len();
        if removed > 0 {
            info!("Cleaned up {} old buffers", removed);
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
        info!("  Memory pressure alerts: {}", global_stats.memory_pressure_alerts);

        let now = Instant::now();

        // ДОБАВЛЯЕМ: Используем поля PooledBuffer
        for (buffer_type, pool) in pools.iter() {
            // ПРОВЕРЯЕМ НАЛИЧИЕ СТАТИСТИКИ
            let buffer_stats = if let Some(stats) = stats.get(buffer_type) {
                stats
            } else {
                continue; // Пропускаем если нет статистики
            };

            // Собираем статистику по полям PooledBuffer
            let total_size: usize = pool.iter().map(|b| b.size).sum();

            let oldest_buffer = pool.iter()
                .filter(|b| !b.is_used)
                .min_by_key(|b| b.created_at)
                .map(|b| now.duration_since(b.created_at).as_secs());

            let avg_age_secs: f64 = if !pool.is_empty() {
                pool.iter()
                    .map(|b| now.duration_since(b.created_at).as_secs_f64())
                    .sum::<f64>() / pool.len() as f64
            } else {
                0.0
            };

            let pool_size = pool.len();
            info!("  {:?}: pool={}, used={}, total_size={}KB, avg_age={:.1}s, oldest={:?}s, hit_rate={:.1}%",
              buffer_type,
              pool_size,
              buffer_stats.currently_used,
              total_size / 1024,
              avg_age_secs,
              oldest_buffer.unwrap_or(0),
              buffer_stats.hit_rate * 100.0);
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

    /// Получение общего количества буферов в пуле
    pub fn total_buffer_count(&self) -> usize {
        let pools = self.pools.read();
        pools.values().map(|p| p.len()).sum()
    }

    /// Получение количества используемых буферов
    pub fn used_buffer_count(&self) -> usize {
        let pools = self.pools.read();
        pools.values()
            .flat_map(|p| p.iter())
            .filter(|b| b.is_used)
            .count()
    }

    /// Получение общего объема выделенной памяти
    pub fn total_memory_allocated(&self) -> usize {
        let global_stats = self.global_stats.lock();
        global_stats.total_memory_allocated
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