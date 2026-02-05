use std::sync::Arc;
use std::collections::VecDeque;
use dashmap::DashMap;
use bytes::BytesMut;
use tracing::{info, debug};
use std::time::{Instant, Duration};
use parking_lot::Mutex;

/// Оптимизированный пул буферов с переиспользованием
pub struct OptimizedBufferPool {
    // Пул больших буферов для операций ввода-вывода
    read_buffers: Arc<Mutex<VecDeque<Vec<u8>>>>,
    write_buffers: Arc<Mutex<VecDeque<Vec<u8>>>>,
    crypto_buffers: Arc<Mutex<VecDeque<Vec<u8>>>>,

    // Пул BytesMut для быстрого создания
    bytes_mut_buffers: Arc<Mutex<VecDeque<BytesMut>>>,

    // Статистика использования
    stats: Arc<DashMap<String, BufferPoolStats>>,

    // Время последней очистки
    last_cleanup: Arc<Mutex<Instant>>,

    // Максимальное количество буферов на тип (сохраняем для фоновой очистки)
    max_buffers_per_type: usize,
}

#[derive(Debug, Clone)]
pub struct BufferPoolStats {
    pub total_allocations: u64,
    pub total_reuses: u64,
    pub current_active: usize,
    pub peak_usage: usize,
    pub last_allocated: Instant,
}

impl OptimizedBufferPool {
    pub fn new(
        read_buffer_size: usize,
        write_buffer_size: usize,
        crypto_buffer_size: usize,
        max_buffers_per_type: usize,
    ) -> Self {
        info!("🚀 Creating optimized buffer pool");

        // Инициализируем пулы с некоторыми предварительно созданными буферами
        let mut read_buffers = VecDeque::with_capacity(max_buffers_per_type);
        let mut write_buffers = VecDeque::with_capacity(max_buffers_per_type);
        let mut crypto_buffers = VecDeque::with_capacity(max_buffers_per_type);
        let mut bytes_mut_buffers = VecDeque::with_capacity(max_buffers_per_type * 2);

        // Создаем несколько буферов заранее
        for _ in 0..max_buffers_per_type / 2 {
            read_buffers.push_back(vec![0u8; read_buffer_size]);
            write_buffers.push_back(vec![0u8; write_buffer_size]);
            crypto_buffers.push_back(vec![0u8; crypto_buffer_size]);
            bytes_mut_buffers.push_back(BytesMut::with_capacity(4096));
        }

        let pool = Self {
            read_buffers: Arc::new(Mutex::new(read_buffers)),
            write_buffers: Arc::new(Mutex::new(write_buffers)),
            crypto_buffers: Arc::new(Mutex::new(crypto_buffers)),
            bytes_mut_buffers: Arc::new(Mutex::new(bytes_mut_buffers)),
            stats: Arc::new(DashMap::new()),
            last_cleanup: Arc::new(Mutex::new(Instant::now())),
            max_buffers_per_type,
        };

        // Инициализируем статистику
        pool.init_stats();

        // Запускаем фоновую очистку
        pool.start_background_cleanup();

        pool
    }

    fn init_stats(&self) {
        self.stats.insert("read".to_string(), BufferPoolStats {
            total_allocations: 0,
            total_reuses: 0,
            current_active: 0,
            peak_usage: 0,
            last_allocated: Instant::now(),
        });

        self.stats.insert("write".to_string(), BufferPoolStats {
            total_allocations: 0,
            total_reuses: 0,
            current_active: 0,
            peak_usage: 0,
            last_allocated: Instant::now(),
        });

        self.stats.insert("crypto".to_string(), BufferPoolStats {
            total_allocations: 0,
            total_reuses: 0,
            current_active: 0,
            peak_usage: 0,
            last_allocated: Instant::now(),
        });

        self.stats.insert("bytes_mut".to_string(), BufferPoolStats {
            total_allocations: 0,
            total_reuses: 0,
            current_active: 0,
            peak_usage: 0,
            last_allocated: Instant::now(),
        });
    }

    /// Получение read буфера
    pub fn acquire_read_buffer(&self) -> Vec<u8> {
        self.acquire_buffer("read", &self.read_buffers, 32 * 1024)
    }

    /// Получение write буфера
    pub fn acquire_write_buffer(&self) -> Vec<u8> {
        self.acquire_buffer("write", &self.write_buffers, 64 * 1024)
    }

    /// Получение crypto буфера
    pub fn acquire_crypto_buffer(&self) -> Vec<u8> {
        self.acquire_buffer("crypto", &self.crypto_buffers, 64 * 1024)
    }

    /// Получение BytesMut буфера
    pub fn acquire_bytes_mut(&self) -> BytesMut {
        let mut buffers = self.bytes_mut_buffers.lock();

        if let Some(mut buffer) = buffers.pop_front() {
            // Переиспользуем существующий буфер
            buffer.clear();

            // Обновляем статистику
            if let Some(mut stats) = self.stats.get_mut("bytes_mut") {
                stats.total_reuses += 1;
                stats.current_active += 1;
                stats.peak_usage = stats.peak_usage.max(stats.current_active);
                stats.last_allocated = Instant::now();
            }

            buffer
        } else {
            // Создаем новый буфер
            if let Some(mut stats) = self.stats.get_mut("bytes_mut") {
                stats.total_allocations += 1;
                stats.current_active += 1;
                stats.peak_usage = stats.peak_usage.max(stats.current_active);
                stats.last_allocated = Instant::now();
            }

            BytesMut::with_capacity(4096)
        }
    }

    fn acquire_buffer(
        &self,
        buffer_type: &str,
        pool: &Arc<Mutex<VecDeque<Vec<u8>>>>,
        default_size: usize,
    ) -> Vec<u8> {
        let mut buffers = pool.lock();

        if let Some(mut buffer) = buffers.pop_front() {
            // Переиспользуем существующий буфер
            buffer.clear();

            // Обновляем статистику
            if let Some(mut stats) = self.stats.get_mut(buffer_type) {
                stats.total_reuses += 1;
                stats.current_active += 1;
                stats.peak_usage = stats.peak_usage.max(stats.current_active);
                stats.last_allocated = Instant::now();
            }

            buffer
        } else {
            // Создаем новый буфер
            if let Some(mut stats) = self.stats.get_mut(buffer_type) {
                stats.total_allocations += 1;
                stats.current_active += 1;
                stats.peak_usage = stats.peak_usage.max(stats.current_active);
                stats.last_allocated = Instant::now();
            }

            vec![0u8; default_size]
        }
    }

    /// Создание буфера с определенным размером
    pub fn create_sized_buffer(&self, size: usize) -> Vec<u8> {
        // Выбираем подходящий пул
        if size <= 1024 {
            let mut buffer = self.acquire_read_buffer();
            buffer.resize(size, 0);
            buffer
        } else if size <= 8192 {
            let mut buffer = self.acquire_write_buffer();
            buffer.resize(size, 0);
            buffer
        } else {
            // Большие буферы создаем напрямую
            vec![0u8; size]
        }
    }

    /// Возврат буфера в пул
    pub fn return_buffer(&self, mut buffer: Vec<u8>, buffer_type: &str) {
        // Очищаем буфер перед возвратом
        buffer.clear();

        match buffer_type {
            "read" => {
                let mut buffers = self.read_buffers.lock();
                buffers.push_back(buffer);
            }
            "write" => {
                let mut buffers = self.write_buffers.lock();
                buffers.push_back(buffer);
            }
            "crypto" => {
                let mut buffers = self.crypto_buffers.lock();
                buffers.push_back(buffer);
            }
            _ => {}
        }

        // Обновляем статистику
        if let Some(mut stats) = self.stats.get_mut(buffer_type) {
            stats.current_active = stats.current_active.saturating_sub(1);
        }
    }

    /// Возврат BytesMut буфера в пул
    pub fn return_bytes_mut(&self, mut buffer: BytesMut) {
        buffer.clear();
        let mut buffers = self.bytes_mut_buffers.lock();
        buffers.push_back(buffer);

        // Обновляем статистику
        if let Some(mut stats) = self.stats.get_mut("bytes_mut") {
            stats.current_active = stats.current_active.saturating_sub(1);
        }
    }

    /// Получение статистики повторного использования буферов
    pub fn get_reuse_rate(&self) -> f64 {
        let stats_map: Vec<_> = self.stats.iter().collect();

        if stats_map.is_empty() {
            return 0.0;
        }

        let mut total_allocations = 0;
        let mut total_reuses = 0;

        for stats in stats_map {
            let stats = stats.value();
            total_allocations += stats.total_allocations;
            total_reuses += stats.total_reuses;
        }

        if total_allocations + total_reuses == 0 {
            return 0.0;
        }

        total_reuses as f64 / (total_allocations + total_reuses) as f64
    }

    /// Получение статистики в структурированном виде
    pub fn get_detailed_stats(&self) -> std::collections::HashMap<String, BufferPoolStats> {
        let mut map = std::collections::HashMap::new();

        for entry in self.stats.iter() {
            map.insert(entry.key().clone(), entry.value().clone());
        }

        map
    }

    /// Добавляем метод для фоновой очистки
    fn start_background_cleanup(&self) {
        let read_buffers = self.read_buffers.clone();
        let write_buffers = self.write_buffers.clone();
        let crypto_buffers = self.crypto_buffers.clone();
        let bytes_mut_buffers = self.bytes_mut_buffers.clone();
        let last_cleanup = self.last_cleanup.clone();
        let max_buffers_per_type = self.max_buffers_per_type;

        tokio::spawn(async move {
            let cleanup_interval = Duration::from_secs(300); // 5 минут

            loop {
                tokio::time::sleep(cleanup_interval).await;

                let now = Instant::now();
                let mut last_cleanup_time = last_cleanup.lock();

                // Проверяем, нужно ли выполнять очистку
                if now.duration_since(*last_cleanup_time) >= cleanup_interval {
                    Self::cleanup_old_buffers_internal(
                        &read_buffers,
                        &write_buffers,
                        &crypto_buffers,
                        &bytes_mut_buffers,
                        max_buffers_per_type,
                    );

                    *last_cleanup_time = now;
                    debug!("✅ Buffer pool background cleanup completed");
                }
            }
        });
    }

    fn cleanup_old_buffers_internal(
        read_buffers: &Arc<Mutex<VecDeque<Vec<u8>>>>,
        write_buffers: &Arc<Mutex<VecDeque<Vec<u8>>>>,
        crypto_buffers: &Arc<Mutex<VecDeque<Vec<u8>>>>,
        bytes_mut_buffers: &Arc<Mutex<VecDeque<BytesMut>>>,
        max_buffers_per_type: usize,
    ) {
        {
            let mut read_guard = read_buffers.lock();
            let mut write_guard = write_buffers.lock();
            let mut crypto_guard = crypto_buffers.lock();

            while read_guard.len() > max_buffers_per_type {
                read_guard.pop_back();
            }

            while write_guard.len() > max_buffers_per_type {
                write_guard.pop_back();
            }

            while crypto_guard.len() > max_buffers_per_type {
                crypto_guard.pop_back();
            }
        }

        // Очищаем BytesMut буферы отдельно (другой тип)
        {
            let mut bytes_mut_guard = bytes_mut_buffers.lock();
            while bytes_mut_guard.len() > max_buffers_per_type * 2 {
                bytes_mut_guard.pop_back();
            }
        }
    }

    /// Добавляем метод для принудительной очистки
    pub fn force_cleanup(&self) {
        let mut last_cleanup = self.last_cleanup.lock();
        Self::cleanup_old_buffers_internal(
            &self.read_buffers,
            &self.write_buffers,
            &self.crypto_buffers,
            &self.bytes_mut_buffers,
            self.max_buffers_per_type,
        );
        *last_cleanup = Instant::now();
        info!("✅ Buffer pool force cleanup completed");
    }

    /// Добавляем метод для получения времени последней очистки
    pub fn get_last_cleanup_time(&self) -> Instant {
        *self.last_cleanup.lock()
    }

    /// Получение информации о максимальном количестве буферов
    pub fn get_max_buffers_per_type(&self) -> usize {
        self.max_buffers_per_type
    }

    /// Получение текущего размера пулов
    pub fn get_pool_sizes(&self) -> PoolSizes {
        PoolSizes {
            read_buffers: self.read_buffers.lock().len(),
            write_buffers: self.write_buffers.lock().len(),
            crypto_buffers: self.crypto_buffers.lock().len(),
            bytes_mut_buffers: self.bytes_mut_buffers.lock().len(),
        }
    }

    /// Получение информации об использовании памяти
    pub fn get_memory_usage(&self) -> MemoryUsage {
        let pools = self.get_pool_sizes();

        // Оцениваем использование памяти (приблизительно)
        let read_memory = pools.read_buffers * 32 * 1024; // 32KB каждый
        let write_memory = pools.write_buffers * 64 * 1024; // 64KB каждый
        let crypto_memory = pools.crypto_buffers * 64 * 1024; // 64KB каждый
        let bytes_mut_memory = pools.bytes_mut_buffers * 4096; // 4KB каждый

        let total_memory = read_memory + write_memory + crypto_memory + bytes_mut_memory;

        MemoryUsage {
            read_memory_kb: read_memory / 1024,
            write_memory_kb: write_memory / 1024,
            crypto_memory_kb: crypto_memory / 1024,
            bytes_mut_memory_kb: bytes_mut_memory / 1024,
            total_memory_kb: total_memory / 1024,
            read_buffers: pools.read_buffers,
            write_buffers: pools.write_buffers,
            crypto_buffers: pools.crypto_buffers,
            bytes_mut_buffers: pools.bytes_mut_buffers,
        }
    }
}

/// Размеры пулов буферов
#[derive(Debug, Clone)]
pub struct PoolSizes {
    pub read_buffers: usize,
    pub write_buffers: usize,
    pub crypto_buffers: usize,
    pub bytes_mut_buffers: usize,
}

/// Использование памяти пулом буферов
#[derive(Debug, Clone)]
pub struct MemoryUsage {
    pub read_memory_kb: usize,
    pub write_memory_kb: usize,
    pub crypto_memory_kb: usize,
    pub bytes_mut_memory_kb: usize,
    pub total_memory_kb: usize,
    pub read_buffers: usize,
    pub write_buffers: usize,
    pub crypto_buffers: usize,
    pub bytes_mut_buffers: usize,
}

impl MemoryUsage {
    pub fn to_string(&self) -> String {
        format!(
            "Total: {:.1} MB (Read: {} buffers, {:.1} KB, Write: {} buffers, {:.1} KB, Crypto: {} buffers, {:.1} KB, BytesMut: {} buffers, {:.1} KB)",
            self.total_memory_kb as f64 / 1024.0,
            self.read_buffers,
            self.read_memory_kb as f64,
            self.write_buffers,
            self.write_memory_kb as f64,
            self.crypto_buffers,
            self.crypto_memory_kb as f64,
            self.bytes_mut_buffers,
            self.bytes_mut_memory_kb as f64,
        )
    }
}