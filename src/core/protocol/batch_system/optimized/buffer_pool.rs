use std::sync::Arc;
use std::collections::{VecDeque, HashMap};
use std::time::{Instant, Duration};
use dashmap::DashMap;
use bytes::BytesMut;
use tracing::{info, debug, warn};
use parking_lot::{Mutex, RwLock};
use tokio::sync::{RwLock as TokioRwLock};  // Добавлено для асинхронного доступа

/// Размерные классы с математическим обоснованием
/// Оптимальные размеры на основе степенного закона
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeClass {
    Small = 0,      // 1KB - частые мелкие операции
    Medium = 1,     // 8KB - средние пакеты
    Large = 2,      // 64KB - большие передачи
    XLarge = 3,     // 256KB - очень большие
    Giant = 4,      // 1MB - гигантские (редко)
}

impl SizeClass {
    /// Оптимальные размеры из степенного закона: S_k = S_0 * r^k
    /// где r ≈ 8 (удвоение в кубе)
    pub fn optimal_size(&self) -> usize {
        match self {
            SizeClass::Small => 1024,      // 1KB
            SizeClass::Medium => 8192,     // 8KB
            SizeClass::Large => 65536,     // 64KB
            SizeClass::XLarge => 262144,   // 256KB
            SizeClass::Giant => 1048576,   // 1MB
        }
    }

    /// Минимальный размер для класса
    pub fn min_size(&self) -> usize {
        match self {
            SizeClass::Small => 1,
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

    /// Определение класса по размеру
    pub fn from_size(size: usize) -> Self {
        match size {
            0..=1024 => SizeClass::Small,
            1025..=8192 => SizeClass::Medium,
            8193..=65536 => SizeClass::Large,
            65537..=262144 => SizeClass::XLarge,
            _ => SizeClass::Giant,
        }
    }

    /// Имя класса
    pub fn name(&self) -> &'static str {
        match self {
            SizeClass::Small => "Small",
            SizeClass::Medium => "Medium",
            SizeClass::Large => "Large",
            SizeClass::XLarge => "XLarge",
            SizeClass::Giant => "Giant",
        }
    }

    /// Индекс для массивов
    pub fn index(&self) -> usize {
        *self as usize
    }

    /// Все классы
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

#[derive(Debug, Clone)]
pub struct SizeDistributionModel {
    /// Параметр формы распределения Парето (α)
    pub alpha: f64,

    /// Минимальный размер (x_m)
    pub x_min: f64,

    /// Средний размер
    pub mean: f64,

    /// Дисперсия
    pub variance: f64,

    /// История размеров для обновления
    pub size_history: VecDeque<usize>,

    /// Максимальный размер истории
    pub max_history: usize,
}

impl SizeDistributionModel {
    pub fn new(max_history: usize) -> Self {
        Self {
            alpha: 2.5,  // Типичное значение для сетевого трафика
            x_min: 64.0,
            mean: 256.0,
            variance: 65536.0,
            size_history: VecDeque::with_capacity(max_history),
            max_history,
        }
    }

    /// Обновление модели на основе нового размера
    pub fn update(&mut self, size: usize) {
        self.size_history.push_back(size);
        if self.size_history.len() > self.max_history {
            self.size_history.pop_front();
        }

        if self.size_history.len() >= 100 {
            self.estimate_parameters();
        }
    }

    /// Оценка параметров распределения Парето методом максимального правдоподобия
    pub fn estimate_parameters(&mut self) {
        let sizes: Vec<f64> = self.size_history.iter()
            .map(|&s| s as f64)
            .collect();

        if sizes.is_empty() {
            return;
        }

        // x_min = min(data)
        self.x_min = sizes.iter().fold(f64::INFINITY, |a, &b| a.min(b));

        // α = n / Σ ln(x_i / x_min)
        let sum_log: f64 = sizes.iter()
            .map(|&x| (x / self.x_min).ln())
            .sum();

        self.alpha = sizes.len() as f64 / sum_log;

        // Среднее для Парето: α·x_min/(α-1) для α > 1
        if self.alpha > 1.0 {
            self.mean = self.alpha * self.x_min / (self.alpha - 1.0);
        }

        // Дисперсия для Парето: α·x_min²/((α-1)²·(α-2)) для α > 2
        if self.alpha > 2.0 {
            self.variance = self.alpha * self.x_min.powi(2) /
                ((self.alpha - 1.0).powi(2) * (self.alpha - 2.0));
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheModel {
    /// Вероятность попадания (hit rate)
    pub hit_rate: f64,

    /// Размер кэша
    pub cache_size: usize,

    /// Время жизни элемента
    pub ttl: Duration,

    /// Коэффициент α для модели независимого ссылочного потока (IRM)
    pub irm_alpha: f64,

    /// Распредеение популярности (Zipf)
    pub zipf_exponent: f64,
}

impl CacheModel {
    pub fn new() -> Self {
        Self {
            hit_rate: 0.0,
            cache_size: 1000,
            ttl: Duration::from_secs(300),
            irm_alpha: 0.8,
            zipf_exponent: 1.2,  // Типичное значение для сетевого трафика
        }
    }

    /// Теоретическая вероятность попадания для Zipf-распределения
    pub fn theoretical_hit_rate(&self, cache_size: usize, total_items: usize) -> f64 {
        if total_items == 0 {
            return 0.0;
        }

        // H(N) - гармоническое число
        let h_total = (1..=total_items).map(|i| 1.0 / (i as f64).powf(self.zipf_exponent)).sum::<f64>();
        let h_cache = (1..=cache_size).map(|i| 1.0 / (i as f64).powf(self.zipf_exponent)).sum::<f64>();

        h_cache / h_total
    }

    /// Оптимальный размер кэша для целевой вероятности попадания
    pub fn optimal_cache_size(&self, target_hit_rate: f64, total_items: usize) -> usize {
        if total_items == 0 {
            return 0;
        }

        let mut low = 1;
        let mut high = total_items;
        let mut best = total_items / 2;

        while low <= high {
            let mid = (low + high) / 2;
            let hit = self.theoretical_hit_rate(mid, total_items);

            if (hit - target_hit_rate).abs() < 0.01 {
                return mid;
            }

            if hit < target_hit_rate {
                low = mid + 1;
            } else {
                high = mid - 1;
                best = mid;
            }
        }

        best
    }
}

#[derive(Debug)]
pub struct PooledBuffer {
    data: Vec<u8>,
    size_class: SizeClass,
    created_at: Instant,
    last_used: Instant,
    usage_count: u32,
    is_used: bool,
    requested_size: usize,
    allocation_time: Duration,
}

impl PooledBuffer {
    pub fn new(size_class: SizeClass) -> Self {
        let start = Instant::now();
        let default_size = size_class.optimal_size();

        Self {
            data: vec![0u8; default_size],
            size_class,
            created_at: Instant::now(),
            last_used: Instant::now(),
            usage_count: 0,
            is_used: false,
            requested_size: default_size,
            allocation_time: start.elapsed(),
        }
    }

    fn with_exact_size(size: usize) -> Self {
        let start = Instant::now();
        let size_class = SizeClass::from_size(size);

        Self {
            data: vec![0u8; size],
            size_class,
            created_at: Instant::now(),
            last_used: Instant::now(),
            usage_count: 0,
            is_used: false,
            requested_size: size,
            allocation_time: start.elapsed(),
        }
    }

    fn can_reuse_for(&self, requested_size: usize) -> bool {
        !self.is_used &&
            self.data.capacity() >= requested_size &&
            self.data.capacity() <= requested_size * 2 &&  // Не более 2x избыточности
            self.usage_count < 1000  // Предотвращение бесконечного переиспользования
    }

    fn prepare_for_reuse(&mut self) {
        self.data.clear();
        self.last_used = Instant::now();
        self.usage_count += 1;
        self.is_used = true;
    }

    fn capacity(&self) -> usize {
        self.data.capacity()
    }

    fn utilization_ratio(&self) -> f64 {
        if self.capacity() == 0 {
            0.0
        } else {
            self.requested_size as f64 / self.capacity() as f64
        }
    }

    fn age(&self) -> Duration {
        Instant::now().duration_since(self.created_at)
    }

    fn idle_time(&self) -> Duration {
        Instant::now().duration_since(self.last_used)
    }
}

#[derive(Debug, Clone)]
pub struct SizeClassStats {
    pub allocations: u64,
    pub reuses: u64,
    pub current_active: usize,
    pub peak_active: usize,
    pub memory_usage: usize,
    pub peak_memory: usize,
    pub avg_reuse_count: f64,
    pub avg_buffer_age_secs: f64,
    pub avg_utilization: f64,
    pub hit_rate: f64,
    pub miss_rate: f64,
    pub allocation_time_avg: Duration,
    pub allocation_time_p95: Duration,
    pub wait_time_avg: Duration,
}

#[derive(Debug, Clone)]
pub struct GlobalStats {
    pub total_allocations: u64,
    pub total_reuses: u64,
    pub total_memory_allocated: usize,
    pub current_hit_rate: f64,
    pub peak_hit_rate: f64,
    pub current_memory_usage: usize,
    pub peak_memory_usage: usize,
    pub last_hit_rate_calc: Instant,
    pub fragmentation_ratio: f64,
}

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
    pub avg_buffer_age_secs: f64,
    pub avg_utilization: f64,
    pub allocation_time_us: f64,
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub max_buffers_per_class: usize,
    pub max_bytes_mut_buffers: usize,
    pub cleanup_interval_secs: u64,
    pub max_buffer_age_secs: u64,
    pub enable_adaptive_pooling: bool,
    pub target_hit_rate: f64,
    pub enable_size_prediction: bool,
    pub preallocation_factor: f64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_buffers_per_class: 200,
            max_bytes_mut_buffers: 500,
            cleanup_interval_secs: 60,
            max_buffer_age_secs: 600,
            enable_adaptive_pooling: true,
            target_hit_rate: 0.85,
            enable_size_prediction: true,
            preallocation_factor: 0.8,
        }
    }
}

pub struct OptimizedBufferPool {
    pub size_class_pools: RwLock<[VecDeque<PooledBuffer>; 5]>,
    bytes_mut_pool: Mutex<VecDeque<BytesMut>>,
    pub size_distribution: RwLock<SizeDistributionModel>,
    pub cache_model: TokioRwLock<CacheModel>,  // Изменено на TokioRwLock для async
    stats: Arc<DashMap<SizeClass, SizeClassStats>>,
    global_stats: Mutex<GlobalStats>,
    allocation_times: Mutex<VecDeque<Duration>>,
    wait_times: Mutex<VecDeque<Duration>>,
    last_cleanup: Mutex<Instant>,
    last_adaptation: Mutex<Instant>,
    config: Arc<PoolConfig>,  // Изменено на Arc для Send
}

impl OptimizedBufferPool {
    pub fn new(
        _read_buffer_size: usize,
        _write_buffer_size: usize,
        _crypto_buffer_size: usize,
        max_buffers_per_type: usize,
    ) -> Self {
        info!("🚀 Creating mathematical buffer pool with optimized size classes");

        let config = Arc::new(PoolConfig {
            max_buffers_per_class: max_buffers_per_type,
            ..Default::default()
        });

        // Инициализация пулов с предварительным выделением
        let mut size_class_pools = [
            VecDeque::with_capacity(config.max_buffers_per_class),
            VecDeque::with_capacity(config.max_buffers_per_class),
            VecDeque::with_capacity(config.max_buffers_per_class),
            VecDeque::with_capacity(config.max_buffers_per_class),
            VecDeque::with_capacity(config.max_buffers_per_class),
        ];

        // Предварительное выделение на основе степенного закона
        for (i, class) in SizeClass::all_classes().iter().enumerate() {
            let initial_count = (config.max_buffers_per_class as f64 * config.preallocation_factor) as usize;
            for _ in 0..initial_count {
                size_class_pools[i].push_back(PooledBuffer::new(*class));
            }
            info!("  {}: {} initial buffers ({} KB)",
                  class.name(),
                  initial_count,
                  class.optimal_size() * initial_count / 1024);
        }

        let size_distribution = SizeDistributionModel::new(1000);
        let cache_model = CacheModel::new();

        let pool = Self {
            size_class_pools: RwLock::new(size_class_pools),
            bytes_mut_pool: Mutex::new(VecDeque::with_capacity(config.max_bytes_mut_buffers)),
            size_distribution: RwLock::new(size_distribution),
            cache_model: TokioRwLock::new(cache_model),
            stats: Arc::new(DashMap::new()),
            global_stats: Mutex::new(GlobalStats {
                total_allocations: 0,
                total_reuses: 0,
                total_memory_allocated: 0,
                current_hit_rate: 0.0,
                peak_hit_rate: 0.0,
                current_memory_usage: 0,
                peak_memory_usage: 0,
                last_hit_rate_calc: Instant::now(),
                fragmentation_ratio: 0.0,
            }),
            allocation_times: Mutex::new(VecDeque::with_capacity(1000)),
            wait_times: Mutex::new(VecDeque::with_capacity(1000)),
            last_cleanup: Mutex::new(Instant::now()),
            last_adaptation: Mutex::new(Instant::now()),
            config,
        };

        pool.init_stats();
        pool.start_background_tasks();

        info!("✅ Buffer pool initialized with size distribution model");

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
                peak_memory: 0,
                avg_reuse_count: 0.0,
                avg_buffer_age_secs: 0.0,
                avg_utilization: 0.0,
                hit_rate: 0.0,
                miss_rate: 0.0,
                allocation_time_avg: Duration::from_micros(0),
                allocation_time_p95: Duration::from_micros(0),
                wait_time_avg: Duration::from_micros(0),
            });
        }
    }

    pub fn acquire_buffer(&self, requested_size: usize) -> Vec<u8> {
        let start_time = Instant::now();
        let size_class = SizeClass::from_size(requested_size);

        // Обновление модели распределения
        if self.config.enable_size_prediction {
            if let Some(mut dist) = self.size_distribution.try_write() {
                dist.update(requested_size);
            }
        }

        let mut wait_time = Duration::from_nanos(0);

        // Попытка получить буфер из пула
        let buffer = self.try_acquire_from_pool(requested_size, size_class, &mut wait_time);

        let allocation_time = start_time.elapsed();

        // Запись времени ожидания и аллокации
        {
            let mut wait_times = self.wait_times.lock();
            wait_times.push_back(wait_time);
            if wait_times.len() > 1000 {
                wait_times.pop_front();
            }
        }

        {
            let mut alloc_times = self.allocation_times.lock();
            alloc_times.push_back(allocation_time);
            if alloc_times.len() > 1000 {
                alloc_times.pop_front();
            }
        }

        buffer
    }

    fn try_acquire_from_pool(&self, requested_size: usize, size_class: SizeClass, wait_time: &mut Duration) -> Vec<u8> {
        let mut global_stats = self.global_stats.lock();
        let mut stats = self.stats.get_mut(&size_class).unwrap();

        let mut pools = self.size_class_pools.write();
        let pool_index = size_class.index();

        let wait_start = Instant::now();

        // 1. Попытка получить буфер точно подходящего класса
        if let Some(index) = pools[pool_index]
            .iter()
            .position(|buf| buf.can_reuse_for(requested_size))
        {
            let mut buffer = pools[pool_index].swap_remove_back(index).unwrap();
            *wait_time = wait_start.elapsed();

            buffer.prepare_for_reuse();

            stats.reuses += 1;
            stats.current_active += 1;
            stats.peak_active = stats.peak_active.max(stats.current_active);
            stats.memory_usage += buffer.capacity();
            stats.peak_memory = stats.peak_memory.max(stats.memory_usage);

            // EMA для среднего количества переиспользований
            stats.avg_reuse_count = stats.avg_reuse_count * 0.9 + buffer.usage_count as f64 * 0.1;
            stats.avg_utilization = stats.avg_utilization * 0.9 + buffer.utilization_ratio() * 0.1;

            global_stats.total_reuses += 1;
            global_stats.current_memory_usage += buffer.capacity();
            global_stats.peak_memory_usage = global_stats.peak_memory_usage.max(global_stats.current_memory_usage);

            debug!("✅ Buffer reuse: class={}, size={}/{}, utilization={:.1}%, age={:?}",
                   size_class.name(), requested_size, buffer.capacity(),
                   buffer.utilization_ratio() * 100.0, buffer.age());

            return buffer.data;
        }

        // 2. Попытка получить буфер из большего класса
        for larger_class in self.get_larger_classes(size_class) {
            let larger_idx = larger_class.index();

            if let Some(index) = pools[larger_idx]
                .iter()
                .position(|buf| buf.can_reuse_for(requested_size))
            {
                let mut buffer = pools[larger_idx].swap_remove_back(index).unwrap();
                *wait_time = wait_start.elapsed();

                buffer.prepare_for_reuse();

                if let Some(mut larger_stats) = self.stats.get_mut(&larger_class) {
                    larger_stats.reuses += 1;
                    larger_stats.current_active += 1;
                    larger_stats.peak_active = larger_stats.peak_active.max(larger_stats.current_active);
                    larger_stats.memory_usage += buffer.capacity();
                    larger_stats.avg_reuse_count = larger_stats.avg_reuse_count * 0.9 + buffer.usage_count as f64 * 0.1;
                }

                global_stats.total_reuses += 1;

                debug!("✅ Buffer reuse from larger class: from={}, to={}, size={}/{}, utilization={:.1}%",
                       larger_class.name(), size_class.name(), requested_size, buffer.capacity(),
                       buffer.utilization_ratio() * 100.0);

                return buffer.data;
            }
        }

        // 3. Создание нового буфера
        *wait_time = wait_start.elapsed();

        let mut buffer = if requested_size <= size_class.optimal_size() {
            PooledBuffer::new(size_class)
        } else {
            PooledBuffer::with_exact_size(requested_size)
        };

        buffer.prepare_for_reuse();

        stats.allocations += 1;
        stats.current_active += 1;
        stats.peak_active = stats.peak_active.max(stats.current_active);
        stats.memory_usage += buffer.capacity();
        stats.peak_memory = stats.peak_memory.max(stats.memory_usage);

        // Обновление среднего возраста
        let total_age_secs = stats.avg_buffer_age_secs * (stats.allocations - 1) as f64;
        stats.avg_buffer_age_secs = (total_age_secs + 0.0) / stats.allocations as f64;

        global_stats.total_allocations += 1;
        global_stats.total_memory_allocated += buffer.capacity();
        global_stats.current_memory_usage += buffer.capacity();
        global_stats.peak_memory_usage = global_stats.peak_memory_usage.max(global_stats.current_memory_usage);

        debug!("🆕 Buffer allocation: class={}, size={}, capacity={}, time={:?}",
               size_class.name(), requested_size, buffer.capacity(), buffer.allocation_time);

        buffer.data
    }

    pub fn return_buffer(&self, mut buffer: Vec<u8>, buffer_type: &str) {
        let capacity = buffer.capacity();
        let size_class = SizeClass::from_size(capacity);

        buffer.clear();

        if self.should_keep_buffer(capacity, size_class) {
            let mut pools = self.size_class_pools.write();
            let pool_index = size_class.index();

            if pools[pool_index].len() < self.config.max_buffers_per_class {
                let pooled_buffer = PooledBuffer {
                    data: buffer,
                    size_class,
                    created_at: Instant::now(),
                    last_used: Instant::now(),
                    usage_count: 1,
                    is_used: false,
                    requested_size: capacity,
                    allocation_time: Duration::from_nanos(0),
                };

                pools[pool_index].push_back(pooled_buffer);

                if let Some(mut stats) = self.stats.get_mut(&size_class) {
                    stats.current_active = stats.current_active.saturating_sub(1);
                    stats.memory_usage = stats.memory_usage.saturating_sub(capacity);
                }

                let mut global_stats = self.global_stats.lock();
                global_stats.current_memory_usage = global_stats.current_memory_usage.saturating_sub(capacity);

                debug!("🔄 Buffer returned: class={}, capacity={}, type={}",
                       size_class.name(), capacity, buffer_type);
            }
        }
    }

    fn get_larger_classes(&self, size_class: SizeClass) -> Vec<SizeClass> {
        match size_class {
            SizeClass::Small => vec![SizeClass::Medium, SizeClass::Large],
            SizeClass::Medium => vec![SizeClass::Large, SizeClass::XLarge],
            SizeClass::Large => vec![SizeClass::XLarge, SizeClass::Giant],
            SizeClass::XLarge => vec![SizeClass::Giant],
            SizeClass::Giant => vec![],
        }
    }

    fn should_keep_buffer(&self, capacity: usize, size_class: SizeClass) -> bool {
        if capacity < 256 {
            return false;
        }

        if let Some(stats) = self.stats.get(&size_class) {
            // Рассчитываем hit rate
            let total_ops = stats.allocations + stats.reuses;
            let hit_rate = if total_ops > 0 {
                stats.reuses as f64 / total_ops as f64
            } else {
                0.0
            };

            // Не сохраняем буферы с низким hit rate
            if hit_rate < 0.3 {
                return false;
            }

            // Для больших буферов более строгие условия
            match size_class {
                SizeClass::Giant | SizeClass::XLarge => {
                    if stats.avg_reuse_count < 2.0 {
                        return false;
                    }
                }
                _ => {}
            }

            // Проверка на переполнение пула
            let pools = self.size_class_pools.read();
            let pool_index = size_class.index();

            if pools[pool_index].len() >= self.config.max_buffers_per_class {
                return false;
            }
        }

        true
    }

    fn start_background_tasks(&self) {
        let pool = self.clone();
        let config = self.config.clone();

        tokio::spawn(async move {
            let cleanup_interval = Duration::from_secs(config.cleanup_interval_secs);
            let adaptation_interval = Duration::from_secs(30);
            let max_age = Duration::from_secs(config.max_buffer_age_secs);

            loop {
                tokio::time::sleep(cleanup_interval).await;

                // Вызываем без удержания блокировок
                pool.cleanup_old_buffers(max_age).await;
                pool.update_statistics();

                if config.enable_adaptive_pooling {
                    pool.adaptive_pool_adjustment().await;
                }

                // Периодическая адаптация
                let now = Instant::now();
                let last_adapt = *pool.last_adaptation.lock();
                if now.duration_since(last_adapt) > adaptation_interval {
                    pool.adapt_pool_configuration().await;
                    *pool.last_adaptation.lock() = now;
                }
            }
        });
    }

    async fn cleanup_old_buffers(&self, max_age: Duration) {
        let now = Instant::now();
        let mut cleaned = 0;
        let mut total_freed = 0;

        // Весь код с блокировкой выполняем синхронно, без await
        let (cleaned, total_freed) = {
            let mut pools = self.size_class_pools.write();

            for (class_idx, pool) in pools.iter_mut().enumerate() {
                let before = pool.len();
                let class = SizeClass::all_classes()[class_idx];

                // Адаптивный минимальный размер пула
                let min_pool_size = match class {
                    SizeClass::Small => 50,
                    SizeClass::Medium => 30,
                    SizeClass::Large => 20,
                    SizeClass::XLarge => 10,
                    SizeClass::Giant => 5,
                };

                pool.retain(|buf| {
                    let is_stale = buf.idle_time() > max_age;
                    let is_old = buf.age() > Duration::from_secs(3600);
                    let low_utilization = buf.utilization_ratio() < 0.5 && buf.usage_count < 5;

                    if (is_stale || (is_old && low_utilization)) && before > min_pool_size {
                        total_freed += buf.capacity();
                        debug!("🧹 Removing buffer: class={}, age={:?}, idle={:?}, util={:.1}%, uses={}",
                           class.name(), buf.age(), buf.idle_time(),
                           buf.utilization_ratio() * 100.0, buf.usage_count);
                        false
                    } else {
                        true
                    }
                });

                cleaned += before - pool.len();
            }
            (cleaned, total_freed)
        }; // Блокировка освобождается здесь

        if cleaned > 0 {
            debug!("🧹 Cleaned up {} old buffers, freed {} bytes", cleaned, total_freed);

            {
                let mut global_stats = self.global_stats.lock();
                global_stats.total_memory_allocated = global_stats.total_memory_allocated.saturating_sub(total_freed);
                global_stats.current_memory_usage = global_stats.current_memory_usage.saturating_sub(total_freed);
            }

            // Теперь можно безопасно делать await, потому что блокировка освобождена
            self.update_class_stats().await;
        }

        *self.last_cleanup.lock() = now;
    }

    fn update_statistics(&self) {
        let mut global_stats = self.global_stats.lock();

        let total_allocations = global_stats.total_allocations;
        let total_reuses = global_stats.total_reuses;

        if total_allocations + total_reuses > 0 {
            let new_hit_rate = total_reuses as f64 / (total_allocations + total_reuses) as f64;
            global_stats.current_hit_rate = new_hit_rate;
            global_stats.peak_hit_rate = global_stats.peak_hit_rate.max(new_hit_rate);
            global_stats.last_hit_rate_calc = Instant::now();

            // Расчёт фрагментации
            let total_active_memory: usize = self.stats.iter()
                .map(|e| e.value().memory_usage)
                .sum();

            let total_allocated = global_stats.total_memory_allocated;
            global_stats.fragmentation_ratio = if total_allocated > 0 {
                1.0 - (total_active_memory as f64 / total_allocated as f64)
            } else {
                0.0
            };

            debug!("📊 Buffer pool stats: hit_rate={:.2}%, fragmentation={:.2}%, memory={:.1}MB",
                   new_hit_rate * 100.0,
                   global_stats.fragmentation_ratio * 100.0,
                   global_stats.current_memory_usage as f64 / 1024.0 / 1024.0);
        }

        // Обновление hit rate для каждого класса
        for mut entry in self.stats.iter_mut() {
            let _class = *entry.key();
            let stats = entry.value_mut();

            let total_ops = stats.allocations + stats.reuses;
            if total_ops > 0 {
                stats.hit_rate = stats.reuses as f64 / total_ops as f64;
                stats.miss_rate = stats.allocations as f64 / total_ops as f64;
            }

            // Расчёт перцентилей времени аллокации
            let alloc_times = self.allocation_times.lock();
            if !alloc_times.is_empty() {
                let mut times: Vec<u64> = alloc_times.iter()
                    .map(|d| d.as_micros() as u64)
                    .collect();
                times.sort_unstable();

                let len = times.len();
                stats.allocation_time_avg = Duration::from_micros(
                    times.iter().sum::<u64>() / len as u64
                );
                stats.allocation_time_p95 = Duration::from_micros(
                    times[len * 95 / 100]
                );
            }

            // Расчёт среднего времени ожидания
            let wait_times = self.wait_times.lock();
            if !wait_times.is_empty() {
                let avg_wait = wait_times.iter().sum::<Duration>().as_micros() as u64
                    / wait_times.len() as u64;
                stats.wait_time_avg = Duration::from_micros(avg_wait);
            }
        }
    }

    async fn update_class_stats(&self) {
        // Захватываем все необходимые данные синхронно
        let class_stats = {
            let pools = self.size_class_pools.read();

            let mut class_stats = Vec::with_capacity(5);
            for (class_idx, pool) in pools.iter().enumerate() {
                let class = SizeClass::all_classes()[class_idx];
                let class_memory: usize = pool.iter().map(|buf| buf.capacity()).sum();

                let (avg_age, avg_util) = if !pool.is_empty() {
                    let total_age: f64 = pool.iter().map(|buf| buf.age().as_secs_f64()).sum();
                    let total_util: f64 = pool.iter().map(|buf| buf.utilization_ratio()).sum();
                    (total_age / pool.len() as f64, total_util / pool.len() as f64)
                } else {
                    (0.0, 0.0)
                };

                class_stats.push((class, class_memory, avg_age, avg_util));
            }
            class_stats
        }; // Блокировка освобождается здесь

        // Обновляем статистику без удержания блокировки
        for (class, class_memory, avg_age, avg_util) in class_stats {
            if let Some(mut stats) = self.stats.get_mut(&class) {
                stats.memory_usage = class_memory;
                stats.avg_buffer_age_secs = avg_age;
                stats.avg_utilization = avg_util;
            }
        }
    }

    async fn adaptive_pool_adjustment(&self) {
        let current_hit_rate = {
            let global_stats = self.global_stats.lock();
            global_stats.current_hit_rate
        };

        if current_hit_rate < self.config.target_hit_rate * 0.8 {
            warn!("📉 Hit rate too low ({:.1}%), increasing pool size",
                  current_hit_rate * 100.0);
            self.increase_pool_sizes().await;
        } else if current_hit_rate > self.config.target_hit_rate * 1.2 {
            debug!("📈 Hit rate high ({:.1}%), can reduce pool size",
                   current_hit_rate * 100.0);
            // Можем уменьшить пул для экономии памяти
            self.optimize_pool_sizes().await;
        }
    }

    async fn increase_pool_sizes(&self) {
        let mut pools = self.size_class_pools.write();

        for (i, class) in SizeClass::all_classes().iter().enumerate() {
            let current_size = pools[i].len();
            let target_size = self.config.max_buffers_per_class;

            if current_size < target_size {
                let to_add = (target_size - current_size).min(20);
                for _ in 0..to_add {
                    pools[i].push_back(PooledBuffer::new(*class));
                }
                debug!("📈 Increased {} pool from {} to {}",
                       class.name(), current_size, current_size + to_add);
            }
        }
    }

    async fn optimize_pool_sizes(&self) {
        let mut pools = self.size_class_pools.write();

        for (i, class) in SizeClass::all_classes().iter().enumerate() {
            if let Some(stats) = self.stats.get(class) {
                // Оптимальный размер пула на основе hit rate
                let optimal_size = if stats.hit_rate > 0.9 {
                    (stats.peak_active as f64 * 1.2) as usize
                } else if stats.hit_rate > 0.7 {
                    (stats.peak_active as f64 * 1.5) as usize
                } else {
                    (stats.peak_active as f64 * 2.0) as usize
                };

                let optimal_size = optimal_size.min(self.config.max_buffers_per_class);
                let current_size = pools[i].len();

                if current_size > optimal_size + 10 {
                    let to_remove = current_size - optimal_size;
                    for _ in 0..to_remove.min(10) {
                        pools[i].pop_back();
                    }
                    debug!("📉 Optimized {} pool from {} to {}",
                           class.name(), current_size, pools[i].len());
                }
            }
        }
    }

    async fn adapt_pool_configuration(&self) {
        // Получаем данные синхронно
        let (alpha, history_len) = if let Some(dist) = self.size_distribution.try_read() {
            (dist.alpha, dist.size_history.len())
        } else {
            return;
        };

        if history_len >= 100 {
            debug!("📊 Size distribution: α={:.2}", alpha);

            // Обновление модели кэша
            let mut cache_model = self.cache_model.write().await;
            cache_model.zipf_exponent = alpha - 1.0;

            // Адаптация целевого hit rate
            let optimal_cache_size = cache_model.optimal_cache_size(
                self.config.target_hit_rate,
                history_len
            );

            // Если у вас есть атомарный счётчик для max_buffers_per_class
            // self.set_max_buffers_per_class(optimal_cache_size.max(100));

            debug!("🎯 Optimal cache size would be: {}", optimal_cache_size);
        }
    }

    pub fn get_reuse_rate(&self) -> f64 {
        let global_stats = self.global_stats.lock();

        if global_stats.total_allocations + global_stats.total_reuses == 0 {
            0.0
        } else {
            global_stats.total_reuses as f64 /
                (global_stats.total_allocations + global_stats.total_reuses) as f64
        }
    }

    pub fn get_detailed_stats(&self) -> HashMap<String, ClassDetailStats> {
        let mut result = HashMap::new();
        let global_stats = self.global_stats.lock();

        for class in SizeClass::all_classes() {
            if let Some(stats) = self.stats.get(&class) {
                let hit_rate = stats.hit_rate;
                let memory_mb = stats.memory_usage as f64 / 1024.0 / 1024.0;
                let alloc_time_us = stats.allocation_time_avg.as_micros() as f64;

                result.insert(class.name().to_string(), ClassDetailStats {
                    class_name: class.name().to_string(),
                    allocations: stats.allocations,
                    reuses: stats.reuses,
                    current_active: stats.current_active,
                    peak_active: stats.peak_active,
                    hit_rate,
                    memory_mb,
                    avg_reuse_count: stats.avg_reuse_count,
                    avg_buffer_age_secs: stats.avg_buffer_age_secs,
                    avg_utilization: stats.avg_utilization,
                    allocation_time_us: alloc_time_us,
                });
            }
        }

        result.insert("Global".to_string(), ClassDetailStats {
            class_name: "Global".to_string(),
            allocations: global_stats.total_allocations,
            reuses: global_stats.total_reuses,
            current_active: 0,
            peak_active: 0,
            hit_rate: global_stats.current_hit_rate,
            memory_mb: global_stats.current_memory_usage as f64 / 1024.0 / 1024.0,
            avg_reuse_count: 0.0,
            avg_buffer_age_secs: 0.0,
            avg_utilization: 1.0 - global_stats.fragmentation_ratio,
            allocation_time_us: 0.0,
        });

        result
    }

    pub async fn force_cleanup(&self) {
        let max_age = if self.config.enable_adaptive_pooling {
            Duration::from_secs(10)  // Агрессивная очистка
        } else {
            Duration::from_secs(0)   // Полная очистка
        };

        self.cleanup_old_buffers(max_age).await;
        self.update_statistics();

        info!("✅ Buffer pool force cleanup completed");
    }
}

impl Clone for OptimizedBufferPool {
    fn clone(&self) -> Self {
        Self {
            size_class_pools: RwLock::new([
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
            ]),
            bytes_mut_pool: Mutex::new(VecDeque::new()),
            size_distribution: RwLock::new(SizeDistributionModel::new(1000)),
            cache_model: TokioRwLock::new(CacheModel::new()),
            stats: Arc::new(DashMap::new()),
            global_stats: Mutex::new(GlobalStats {
                total_allocations: 0,
                total_reuses: 0,
                total_memory_allocated: 0,
                current_hit_rate: 0.0,
                peak_hit_rate: 0.0,
                current_memory_usage: 0,
                peak_memory_usage: 0,
                last_hit_rate_calc: Instant::now(),
                fragmentation_ratio: 0.0,
            }),
            allocation_times: Mutex::new(VecDeque::new()),
            wait_times: Mutex::new(VecDeque::new()),
            last_cleanup: Mutex::new(Instant::now()),
            last_adaptation: Mutex::new(Instant::now()),
            config: self.config.clone(),
        }
    }
}