use std::sync::Arc;
use std::collections::VecDeque;
use dashmap::DashMap;
use bytes::BytesMut;
use tracing::{info, debug, warn};
use std::time::{Instant, Duration};
use parking_lot::{Mutex, RwLock};

/// Размерные классы для оптимизации переиспользования буферов
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeClass {
    /// Маленькие буферы: 64B - 1KB
    Small,
    /// Средние буферы: 1KB - 8KB
    Medium,
    /// Большие буферы: 8KB - 64KB
    Large,
    /// Очень большие буферы: 64KB - 256KB
    XLarge,
    /// Гигантские буферы: 256KB - 1MB
    Giant,
}

impl SizeClass {
    /// Определяем размерный класс по требуемому размеру
    pub fn from_size(size: usize) -> Self {
        match size {
            0..=1024 => SizeClass::Small,        // до 1KB
            1025..=8192 => SizeClass::Medium,    // до 8KB
            8193..=65536 => SizeClass::Large,    // до 64KB
            65537..=262144 => SizeClass::XLarge, // до 256KB
            _ => SizeClass::Giant,               // свыше 256KB
        }
    }

    /// Получаем размер по умолчанию для класса
    pub fn default_size(&self) -> usize {
        match self {
            SizeClass::Small => 1024,    // 1KB
            SizeClass::Medium => 8192,   // 8KB
            SizeClass::Large => 65536,   // 64KB
            SizeClass::XLarge => 262144, // 256KB
            SizeClass::Giant => 1048576, // 1MB
        }
    }

    /// Минимальный размер для класса
    pub fn min_size(&self) -> usize {
        match self {
            SizeClass::Small => 64,
            SizeClass::Medium => 1025,
            SizeClass::Large => 8193,
            SizeClass::XLarge => 65537,
            SizeClass::Giant => 262145,
        }
    }

    /// Максимальный размер для класса
    pub fn max_size(&self) -> usize {
        match self {
            SizeClass::Small => 1024,
            SizeClass::Medium => 8192,
            SizeClass::Large => 65536,
            SizeClass::XLarge => 262144,
            SizeClass::Giant => 1048576,
        }
    }

    /// Имя класса для отладки
    pub fn name(&self) -> &'static str {
        match self {
            SizeClass::Small => "Small",
            SizeClass::Medium => "Medium",
            SizeClass::Large => "Large",
            SizeClass::XLarge => "XLarge",
            SizeClass::Giant => "Giant",
        }
    }

    /// Преобразование в usize для индексации массива
    pub fn as_usize(&self) -> usize {
        *self as usize
    }

    /// Все размерные классы
    pub fn all_classes() -> [SizeClass; 5] {
        [
            SizeClass::Small,
            SizeClass::Medium,
            SizeClass::Large,
            SizeClass::XLarge,
            SizeClass::Giant,
        ]
    }
}

/// Буфер с метаданными для пула
#[derive(Debug)]
struct PooledBuffer {
    data: Vec<u8>,
    size_class: SizeClass,
    created_at: Instant,
    last_used: Instant,
    usage_count: u32,
    is_used: bool,
}

impl PooledBuffer {
    /// Создание нового буфера
    fn new(size_class: SizeClass) -> Self {
        let default_size = size_class.default_size();
        Self {
            data: vec![0u8; default_size],
            size_class,
            created_at: Instant::now(),
            last_used: Instant::now(),
            usage_count: 0,
            is_used: false,
        }
    }

    /// Создание буфера точного размера
    fn with_exact_size(size: usize) -> Self {
        let size_class = SizeClass::from_size(size);
        Self {
            data: vec![0u8; size],
            size_class,
            created_at: Instant::now(),
            last_used: Instant::now(),
            usage_count: 0,
            is_used: false,
        }
    }

    /// Проверка, подходит ли буфер для использования
    fn can_reuse_for(&self, requested_size: usize) -> bool {
        !self.is_used &&
            self.data.capacity() >= requested_size &&
            self.data.capacity() <= requested_size * 2 // Не более чем в 2 раза больше
    }

    /// Очистка буфера перед повторным использованием
    fn prepare_for_reuse(&mut self) {
        self.data.clear();
        self.last_used = Instant::now();
        self.usage_count += 1;
        self.is_used = true;
    }

    /// Получение доступной емкости
    fn capacity(&self) -> usize {
        self.data.capacity()
    }

    /// Получение информации о буфере для мониторинга
    pub fn get_info(&self) -> PooledBufferInfo {
        PooledBufferInfo {
            size_class: self.size_class,
            data_size: self.data.len(),
            capacity: self.data.capacity(),
            created_at: self.created_at,
            last_used: self.last_used,
            usage_count: self.usage_count,
            is_used: self.is_used,
            age_seconds: self.created_at.elapsed().as_secs(),
            idle_seconds: self.last_used.elapsed().as_secs(),
        }
    }

    /// Проверка, является ли буфер устаревшим
    pub fn is_stale(&self, max_age: Duration) -> bool {
        !self.is_used && Instant::now().duration_since(self.last_used) > max_age
    }
}

/// Информация о буфере для мониторинга
#[derive(Debug, Clone)]
pub struct PooledBufferInfo {
    pub size_class: SizeClass,
    pub data_size: usize,
    pub capacity: usize,
    pub created_at: Instant,
    pub last_used: Instant,
    pub usage_count: u32,
    pub is_used: bool,
    pub age_seconds: u64,
    pub idle_seconds: u64,
}

/// Оптимизированный пул буферов с размерными классами
pub struct OptimizedBufferPool {
    // Пул буферов по размерным классам
    size_class_pools: RwLock<[VecDeque<PooledBuffer>; 5]>,

    // Пул BytesMut для быстрого создания
    bytes_mut_pool: Mutex<VecDeque<BytesMut>>,

    // Статистика использования по размерным классам
    stats: Arc<DashMap<SizeClass, SizeClassStats>>,

    // Общая статистика
    global_stats: Mutex<GlobalStats>,

    // Время последней очистки
    last_cleanup: Mutex<Instant>,

    // Настройки пула
    config: PoolConfig,
}

/// Статистика для размерного класса
#[derive(Debug, Clone)]
pub struct SizeClassStats {
    pub allocations: u64,
    pub reuses: u64,
    pub current_active: usize,
    pub peak_active: usize,
    pub memory_usage: usize, // в байтах
    pub avg_reuse_count: f64,
}

/// Глобальная статистика пула
#[derive(Debug, Clone)]
pub struct GlobalStats {
    pub total_allocations: u64,
    pub total_reuses: u64,
    pub total_memory_allocated: usize,
    pub current_hit_rate: f64,
    pub peak_hit_rate: f64,
    pub last_hit_rate_calc: Instant,
}

/// Конфигурация пула
#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub max_buffers_per_class: usize,
    pub max_bytes_mut_buffers: usize,
    pub cleanup_interval_secs: u64,
    pub max_buffer_age_secs: u64,
    pub enable_adaptive_pooling: bool,
    pub target_hit_rate: f64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_buffers_per_class: 100,
            max_bytes_mut_buffers: 200,
            cleanup_interval_secs: 300, // 5 минут
            max_buffer_age_secs: 3600,  // 1 час
            enable_adaptive_pooling: true,
            target_hit_rate: 0.85,
        }
    }
}

impl OptimizedBufferPool {
    pub fn new(
        _read_buffer_size: usize,
        _write_buffer_size: usize,
        _crypto_buffer_size: usize,
        max_buffers_per_type: usize,
    ) -> Self {
        info!("🚀 Creating optimized buffer pool with size classes");

        let config = PoolConfig {
            max_buffers_per_class: max_buffers_per_type,
            ..Default::default()
        };

        // Инициализируем пулы для каждого размерного класса
        let size_class_pools = RwLock::new([
            VecDeque::with_capacity(max_buffers_per_type), // Small
            VecDeque::with_capacity(max_buffers_per_type), // Medium
            VecDeque::with_capacity(max_buffers_per_type), // Large
            VecDeque::with_capacity(max_buffers_per_type), // XLarge
            VecDeque::with_capacity(max_buffers_per_type), // Giant
        ]);

        // Предварительно создаем некоторое количество буферов для каждого класса
        {
            let mut pools = size_class_pools.write();
            for (i, class) in SizeClass::all_classes().iter().enumerate() {
                // Создаем начальный набор буферов (25% от максимума)
                let initial_count = max_buffers_per_type / 4;
                for _ in 0..initial_count {
                    pools[i].push_back(PooledBuffer::new(*class));
                }
                info!("  {}: {} initial buffers", class.name(), initial_count);
            }
        }

        let pool = Self {
            size_class_pools,
            bytes_mut_pool: Mutex::new(VecDeque::with_capacity(config.max_bytes_mut_buffers)),
            stats: Arc::new(DashMap::new()),
            global_stats: Mutex::new(GlobalStats {
                total_allocations: 0,
                total_reuses: 0,
                total_memory_allocated: 0,
                current_hit_rate: 0.0,
                peak_hit_rate: 0.0,
                last_hit_rate_calc: Instant::now(),
            }),
            last_cleanup: Mutex::new(Instant::now()),
            config,
        };

        // Инициализируем статистику
        pool.init_stats();

        // Запускаем фоновые задачи
        pool.start_background_tasks();

        pool
    }

    fn init_stats(&self) {
        for class in SizeClass::all_classes() {
            self.stats.insert(class, SizeClassStats {
                allocations: 0,
                reuses: 0,
                current_active: 0,
                peak_active: 0,
                memory_usage: 0,
                avg_reuse_count: 0.0,
            });
        }
    }

    /// Получение буфера оптимального размера
    pub fn acquire_buffer(&self, requested_size: usize) -> Vec<u8> {
        let size_class = SizeClass::from_size(requested_size);
        let start_time = Instant::now();

        let mut global_stats = self.global_stats.lock();
        let mut stats = self.stats.get_mut(&size_class).unwrap();

        // Ищем подходящий буфер в пуле
        let mut pools = self.size_class_pools.write();
        let pool_index = size_class.as_usize();

        // Пытаемся найти буфер в своем классем
        if let Some(index) = pools[pool_index]
            .iter()
            .position(|buf| buf.can_reuse_for(requested_size))
        {
            // Нашли подходящий буфер
            let mut buffer = pools[pool_index].swap_remove_back(index).unwrap();
            buffer.prepare_for_reuse();

            // Обновляем статистику
            stats.reuses += 1;
            stats.current_active += 1;
            stats.peak_active = stats.peak_active.max(stats.current_active);
            global_stats.total_reuses += 1;

            debug!("✅ Buffer reuse: class={}, size={}, capacity={}, time={:?}",
                   size_class.name(), requested_size, buffer.capacity(), start_time.elapsed());

            // Возвращаем буфер
            return buffer.data;
        }

        // Не нашли в своем классе, ищем в следующем большем классе
        for larger_class in self.get_larger_classes(size_class) {
            let larger_pool_index = larger_class.as_usize();

            if let Some(index) = pools[larger_pool_index]
                .iter()
                .position(|buf| buf.can_reuse_for(requested_size))
            {
                // Нашли буфер в большем классе
                let mut buffer = pools[larger_pool_index].swap_remove_back(index).unwrap();
                buffer.prepare_for_reuse();

                // Обновляем статистику для большего класса
                if let Some(mut larger_stats) = self.stats.get_mut(&larger_class) {
                    larger_stats.reuses += 1;
                    larger_stats.current_active += 1;
                    larger_stats.peak_active = larger_stats.peak_active.max(larger_stats.current_active);
                }

                global_stats.total_reuses += 1;

                debug!("✅ Buffer reuse from larger class: from={}, to={}, size={}, capacity={}",
                       larger_class.name(), size_class.name(), requested_size, buffer.capacity());

                return buffer.data;
            }
        }

        // Не нашли подходящий буфер, создаем новый
        let mut buffer = if requested_size <= size_class.default_size() {
            PooledBuffer::new(size_class)
        } else {
            PooledBuffer::with_exact_size(requested_size)
        };

        buffer.prepare_for_reuse();

        // Обновляем статистику
        stats.allocations += 1;
        stats.current_active += 1;
        stats.peak_active = stats.peak_active.max(stats.current_active);
        stats.memory_usage += buffer.capacity();

        global_stats.total_allocations += 1;
        global_stats.total_memory_allocated += buffer.capacity();

        debug!("🆕 Buffer allocation: class={}, size={}, capacity={}, time={:?}",
               size_class.name(), requested_size, buffer.capacity(), start_time.elapsed());

        buffer.data
    }

    /// Получение read буфера (совместимость со старым интерфейсом)
    pub fn acquire_read_buffer(&self) -> Vec<u8> {
        self.acquire_buffer(32 * 1024) // 32KB для чтения
    }

    /// Получение write буфера (совместимость со старым интерфейсом)
    pub fn acquire_write_buffer(&self) -> Vec<u8> {
        self.acquire_buffer(64 * 1024) // 64KB для записи
    }

    /// Получение crypto буфера (совместимость со старым интерфейсом)
    pub fn acquire_crypto_buffer(&self) -> Vec<u8> {
        self.acquire_buffer(64 * 1024) // 64KB для криптографии
    }

    /// Получение BytesMut буфера
    pub fn acquire_bytes_mut(&self) -> BytesMut {
        let mut pool = self.bytes_mut_pool.lock();

        if let Some(mut buffer) = pool.pop_front() {
            buffer.clear();
            buffer
        } else {
            BytesMut::with_capacity(4096)
        }
    }

    /// Возврат буфера в пул
    pub fn return_buffer(&self, mut buffer: Vec<u8>, _buffer_type: &str) {
        let capacity = buffer.capacity();
        let size_class = SizeClass::from_size(capacity);

        // Очищаем буфер
        buffer.clear();

        // Проверяем, стоит ли сохранять этот буфер
        if self.should_keep_buffer(capacity, size_class) {
            let mut pools = self.size_class_pools.write();
            let pool_index = size_class.as_usize();

            // Проверяем, не переполнен ли пул
            if pools[pool_index].len() < self.config.max_buffers_per_class {
                let pooled_buffer = PooledBuffer {
                    data: buffer,
                    size_class,
                    created_at: Instant::now(),
                    last_used: Instant::now(),
                    usage_count: 1,
                    is_used: false,
                };

                pools[pool_index].push_back(pooled_buffer);

                // Обновляем статистику
                if let Some(mut stats) = self.stats.get_mut(&size_class) {
                    stats.current_active = stats.current_active.saturating_sub(1);
                }
            } else {
                // Пул переполнен, освобождаем память
                drop(buffer);
            }
        } else {
            // Не стоит сохранять, освобождаем память
            drop(buffer);
        }
    }

    /// Возврат BytesMut буфера
    pub fn return_bytes_mut(&self, mut buffer: BytesMut) {
        buffer.clear();

        let mut pool = self.bytes_mut_pool.lock();
        if pool.len() < self.config.max_bytes_mut_buffers {
            pool.push_back(buffer);
        }
        // Иначе буфер будет автоматически освобожден
    }

    /// Получение статистики повторного использования
    pub fn get_reuse_rate(&self) -> f64 {
        let global_stats = self.global_stats.lock();

        if global_stats.total_allocations + global_stats.total_reuses == 0 {
            return 0.0;
        }

        global_stats.total_reuses as f64 /
            (global_stats.total_allocations + global_stats.total_reuses) as f64
    }

    /// Получение детальной статистики
    pub fn get_detailed_stats(&self) -> std::collections::HashMap<String, ClassDetailStats> {
        let mut result = std::collections::HashMap::new();
        let global_stats = self.global_stats.lock();

        for class in SizeClass::all_classes() {
            if let Some(stats) = self.stats.get(&class) {
                let hit_rate = if stats.allocations + stats.reuses > 0 {
                    stats.reuses as f64 / (stats.allocations + stats.reuses) as f64
                } else {
                    0.0
                };

                let memory_mb = stats.memory_usage as f64 / 1024.0 / 1024.0;

                result.insert(class.name().to_string(), ClassDetailStats {
                    class_name: class.name().to_string(),
                    allocations: stats.allocations,
                    reuses: stats.reuses,
                    current_active: stats.current_active,
                    peak_active: stats.peak_active,
                    hit_rate,
                    memory_mb,
                    avg_reuse_count: stats.avg_reuse_count,
                });
            }
        }

        // Добавляем глобальную статистику
        result.insert("Global".to_string(), ClassDetailStats {
            class_name: "Global".to_string(),
            allocations: global_stats.total_allocations,
            reuses: global_stats.total_reuses,
            current_active: 0,
            peak_active: 0,
            hit_rate: global_stats.current_hit_rate,
            memory_mb: global_stats.total_memory_allocated as f64 / 1024.0 / 1024.0,
            avg_reuse_count: 0.0,
        });

        result
    }

    /// Получение следующих больших классов для поиска буферов
    fn get_larger_classes(&self, size_class: SizeClass) -> Vec<SizeClass> {
        match size_class {
            SizeClass::Small => vec![SizeClass::Medium, SizeClass::Large],
            SizeClass::Medium => vec![SizeClass::Large, SizeClass::XLarge],
            SizeClass::Large => vec![SizeClass::XLarge, SizeClass::Giant],
            SizeClass::XLarge => vec![SizeClass::Giant],
            SizeClass::Giant => vec![],
        }
    }

    /// Проверка, стоит ли сохранять буфер
    fn should_keep_buffer(&self, capacity: usize, size_class: SizeClass) -> bool {
        // Не сохраняем слишком маленькие буферы
        if capacity < 256 {
            return false;
        }

        // Проверяем статистику использования для этого класса
        if let Some(stats) = self.stats.get(&size_class) {
            let hit_rate = if stats.allocations + stats.reuses > 0 {
                stats.reuses as f64 / (stats.allocations + stats.reuses) as f64
            } else {
                0.0
            };

            // Если hit rate низкий, возможно, не стоит сохранять много буферов этого класса
            if hit_rate < 0.3 {
                return false;
            }
        }

        true
    }

    /// Запуск фоновых задач
    fn start_background_tasks(&self) {
        let pool = self.clone();

        // Задача очистки старых буферов
        tokio::spawn(async move {
            let cleanup_interval = Duration::from_secs(pool.config.cleanup_interval_secs);
            let max_age = Duration::from_secs(pool.config.max_buffer_age_secs);

            loop {
                tokio::time::sleep(cleanup_interval).await;
                pool.cleanup_old_buffers(max_age);
                pool.update_hit_rate();
                pool.adaptive_pool_adjustment();
            }
        });
    }

    /// Очистка старых буферов
    fn cleanup_old_buffers(&self, max_age: Duration) {
        let now = Instant::now();
        let mut cleaned = 0;
        let mut total_freed = 0;

        let mut pools = self.size_class_pools.write();

        for (class_idx, pool) in pools.iter_mut().enumerate() {
            let before = pool.len();
            let class = SizeClass::all_classes()[class_idx];

            pool.retain(|buf| {
                if buf.is_stale(max_age) {
                    total_freed += buf.capacity();
                    let info = buf.get_info();
                    debug!("🧹 Cleaning up stale buffer: class={}, age={}s, idle={}s, usage={}",
                       class.name(), info.age_seconds, info.idle_seconds, info.usage_count);
                    false
                } else {
                    true
                }
            });

            cleaned += before - pool.len();
        }

        if cleaned > 0 {
            debug!("🧹 Cleaned up {} old buffers, freed {} bytes", cleaned, total_freed);

            // Обновляем метрики
            let mut global_stats = self.global_stats.lock();
            global_stats.total_memory_allocated = global_stats.total_memory_allocated.saturating_sub(total_freed);

            // Обновляем метрики по классам
            for (class_idx, pool) in pools.iter().enumerate() {
                let class = SizeClass::all_classes()[class_idx];
                let class_memory: usize = pool.iter().map(|buf| buf.data.capacity()).sum();

                if let Some(mut stats) = self.stats.get_mut(&class) {
                    stats.memory_usage = class_memory;
                }
            }
        }

        *self.last_cleanup.lock() = now;
    }

    /// Обновление показателя hit rate
    fn update_hit_rate(&self) {
        let mut global_stats = self.global_stats.lock();

        if global_stats.total_allocations + global_stats.total_reuses > 0 {
            let new_hit_rate = global_stats.total_reuses as f64 /
                (global_stats.total_allocations + global_stats.total_reuses) as f64;

            global_stats.current_hit_rate = new_hit_rate;
            global_stats.peak_hit_rate = global_stats.peak_hit_rate.max(new_hit_rate);
            global_stats.last_hit_rate_calc = Instant::now();

            debug!("📊 Hit rate updated: {:.2}% (peak: {:.2}%)",
                   new_hit_rate * 100.0, global_stats.peak_hit_rate * 100.0);
        }
    }

    /// Адаптивная регулировка пула
    fn adaptive_pool_adjustment(&self) {
        if !self.config.enable_adaptive_pooling {
            return;
        }

        let global_stats = self.global_stats.lock();
        let current_hit_rate = global_stats.current_hit_rate;
        let target_hit_rate = self.config.target_hit_rate;

        if current_hit_rate < target_hit_rate * 0.8 {
            // Hit rate слишком низкий, возможно нужно увеличить размер пула
            warn!("📉 Hit rate too low ({:.2}%), consider adjusting buffer sizes",
                  current_hit_rate * 100.0);
        } else if current_hit_rate > target_hit_rate * 1.2 {
            // Hit rate слишком высокий, возможно пул слишком большой
            debug!("📈 Hit rate excellent ({:.2}%), pool well sized",
                   current_hit_rate * 100.0);
        }
    }

    /// Принудительная очистка
    pub fn force_cleanup(&self) {
        let max_age = Duration::from_secs(0); // Очищаем все неиспользуемые буферы
        self.cleanup_old_buffers(max_age);
        info!("✅ Buffer pool force cleanup completed");
    }

    /// Получение информации об использовании памяти
    pub fn get_memory_usage(&self) -> MemoryUsage {
        let pools = self.size_class_pools.read();
        let bytes_mut_pool = self.bytes_mut_pool.lock();

        let mut memory_by_class = std::collections::HashMap::new();
        let mut total_memory = 0;

        // Считаем память по классам
        for (i, class) in SizeClass::all_classes().iter().enumerate() {
            let memory: usize = pools[i]
                .iter()
                .map(|buf| buf.capacity())
                .sum();

            memory_by_class.insert(*class, memory);
            total_memory += memory;
        }

        // Память для BytesMut
        let bytes_mut_memory: usize = bytes_mut_pool
            .iter()
            .map(|buf| buf.capacity())
            .sum();

        total_memory += bytes_mut_memory;

        MemoryUsage {
            memory_by_class,
            bytes_mut_memory_kb: bytes_mut_memory / 1024,
            total_memory_kb: total_memory / 1024,
            buffers_by_class: pools.iter().map(|p| p.len()).collect::<Vec<_>>(),
            bytes_mut_buffers: bytes_mut_pool.len(),
        }
    }

    /// Создание буфера определенного размера
    pub fn create_sized_buffer(&self, size: usize) -> Vec<u8> {
        self.acquire_buffer(size)
    }
}

/// Детальная статистика класса
#[derive(Debug, Clone)]
pub struct ClassDetailStats {
    pub class_name: String,
    pub allocations: u64,
    pub reuses: u64,
    pub current_active: usize,
    pub peak_active: usize,
    pub hit_rate: f64,
    pub memory_mb: f64,
    pub avg_reuse_count: f64,
}

/// Использование памяти
#[derive(Debug, Clone)]
pub struct MemoryUsage {
    pub memory_by_class: std::collections::HashMap<SizeClass, usize>,
    pub bytes_mut_memory_kb: usize,
    pub total_memory_kb: usize,
    pub buffers_by_class: Vec<usize>,
    pub bytes_mut_buffers: usize,
}

impl MemoryUsage {
    pub fn to_string(&self) -> String {
        let mut result = String::new();
        result.push_str(&format!("Total: {:.1} MB\n", self.total_memory_kb as f64 / 1024.0));

        for (class, &memory) in &self.memory_by_class {
            if memory > 0 {
                let class_index = class.as_usize();
                if class_index < self.buffers_by_class.len() {
                    result.push_str(&format!("  {}: {} buffers, {:.1} KB\n",
                                             class.name(),
                                             self.buffers_by_class[class_index],
                                             memory as f64 / 1024.0));
                }
            }
        }

        if self.bytes_mut_memory_kb > 0 {
            result.push_str(&format!("  BytesMut: {} buffers, {:.1} KB\n",
                                     self.bytes_mut_buffers,
                                     self.bytes_mut_memory_kb as f64));
        }

        result
    }
}

impl Clone for OptimizedBufferPool {
    fn clone(&self) -> Self {
        // Для клонирования создаем новый пул с теми же параметрами
        // Не копируем существующие буферы, так как они могут быть в использовании
        Self {
            size_class_pools: RwLock::new([
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
            ]),
            bytes_mut_pool: Mutex::new(VecDeque::new()),
            stats: Arc::new(DashMap::new()),
            global_stats: Mutex::new(GlobalStats {
                total_allocations: 0,
                total_reuses: 0,
                total_memory_allocated: 0,
                current_hit_rate: 0.0,
                peak_hit_rate: 0.0,
                last_hit_rate_calc: Instant::now(),
            }),
            last_cleanup: Mutex::new(Instant::now()),
            config: self.config.clone(),
        }
    }
}