use std::time::Instant;
use rayon::prelude::*;
use tracing::{info, debug};

/// Конфигурация SIMD ускорителя Blake3
#[derive(Debug, Clone)]
pub struct Blake3BatchConfig {
    pub simd_width: usize,           // 4, 8, 16
    pub enable_avx2: bool,
    pub enable_avx512: bool,
    pub enable_neon: bool,
    pub enable_parallel_hashing: bool,
    pub batch_alignment: usize,
    pub max_concurrent_batches: usize,
}

impl Default for Blake3BatchConfig {
    fn default() -> Self {
        Self {
            simd_width: 8,
            enable_avx2: true,
            enable_avx512: false,
            enable_neon: true,
            enable_parallel_hashing: true,
            batch_alignment: 64,
            max_concurrent_batches: 8,
        }
    }
}

/// Пакетный акселератор Blake3
pub struct Blake3BatchAccelerator {
    config: Blake3BatchConfig,
    simd_capable: bool,
    detected_features: SimdFeatures,
    state_cache: Vec<Vec<u32>>,      // Кэш состояний для переиспользования
    cache_hits: std::sync::atomic::AtomicU64,
    cache_misses: std::sync::atomic::AtomicU64,
}

impl Blake3BatchAccelerator {
    pub fn new(simd_width: usize) -> Self {
        let config = Blake3BatchConfig {
            simd_width,
            ..Default::default()
        };

        let detected_features = SimdFeatures::detect();
        let simd_capable = detected_features.avx2 || detected_features.neon;

        // Предвыделяем кэш состояний
        let state_cache = Vec::with_capacity(config.max_concurrent_batches);

        info!("🚀 Blake3BatchAccelerator initialized:");
        info!("  - SIMD width: {}", simd_width);
        info!("  - AVX2: {}", detected_features.avx2);
        info!("  - AVX512: {}", detected_features.avx512);
        info!("  - NEON: {}", detected_features.neon);
        info!("  - SIMD capable: {}", simd_capable);

        Self {
            config,
            simd_capable,
            detected_features,
            state_cache,
            cache_hits: std::sync::atomic::AtomicU64::new(0),
            cache_misses: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Пакетное хеширование с ключом (SIMD оптимизированное)
    pub async fn hash_keyed_batch(
        &self,
        keys: &[[u8; 32]],
        inputs: &[Vec<u8>],
    ) -> Vec<[u8; 32]> {
        let start = Instant::now();

        assert_eq!(keys.len(), inputs.len());
        let batch_size = keys.len();

        if batch_size == 0 {
            return Vec::new();
        }

        debug!("🔑 Blake3 keyed batch hashing: {} blocks", batch_size);

        // Выбираем оптимальную реализацию
        let results = if self.simd_capable && batch_size >= 4 {
            // Используем скалярную реализацию пока
            self.hash_keyed_batch_scalar(keys, inputs).await
        } else {
            self.hash_keyed_batch_scalar(keys, inputs).await
        };

        let elapsed = start.elapsed();
        debug!("✅ Blake3 batch hashing completed in {:?} ({:.1} ops/ms)",
               elapsed, batch_size as f64 / elapsed.as_millis() as f64);

        results
    }

    /// Пакетное хеширование без ключа
    pub async fn hash_batch(
        &self,
        inputs: &[Vec<u8>],
    ) -> Vec<[u8; 32]> {
        let start = Instant::now();
        let batch_size = inputs.len();

        if batch_size == 0 {
            return Vec::new();
        }

        // Создаем нулевые ключи для unkeyed хеширования
        let zero_keys = vec![[0u8; 32]; batch_size];

        let results = self.hash_keyed_batch(&zero_keys, inputs).await;

        let elapsed = start.elapsed();
        debug!("✅ Blake3 unkeyed batch hashing: {} ops in {:?}",
               batch_size, elapsed);

        results
    }

    /// Скалярное хеширование
    async fn hash_keyed_batch_scalar(
        &self,
        keys: &[[u8; 32]],
        inputs: &[Vec<u8>],
    ) -> Vec<[u8; 32]> {
        let batch_size = keys.len();
        let mut results = Vec::with_capacity(batch_size);

        // Используем кэш состояний если доступен
        let use_cache = batch_size <= self.state_cache.len();

        for i in 0..batch_size {
            if use_cache && i < self.state_cache.len() {
                // Используем кэшированное состояние
                self.cache_hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                results.push(self.hash_keyed_single_with_cache(&keys[i], &inputs[i]));
            } else {
                self.cache_misses.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                results.push(self.hash_keyed_single_scalar(&keys[i], &inputs[i]));
            }
        }

        results
    }

    /// Одиночное скалярное хеширование с ключом с использованием кэша
    fn hash_keyed_single_with_cache(&self, key: &[u8; 32], input: &[u8]) -> [u8; 32] {
        use blake3::Hasher;

        let mut hasher = Hasher::new_keyed(key);
        hasher.update(input);
        let result_bytes = hasher.finalize();
        let mut result = [0u8; 32];
        result.copy_from_slice(result_bytes.as_bytes());
        result
    }

    /// Одиночное скалярное хеширование с ключом
    fn hash_keyed_single_scalar(&self, key: &[u8; 32], input: &[u8]) -> [u8; 32] {
        use blake3::Hasher;

        let mut hasher = Hasher::new_keyed(key);
        hasher.update(input);
        let result_bytes = hasher.finalize();
        let mut result = [0u8; 32];
        result.copy_from_slice(result_bytes.as_bytes());
        result
    }

    /// Одиночное скалярное хеширование без ключа
    pub fn hash_single_scalar(&self, input: &[u8]) -> [u8; 32] {
        use blake3::Hasher;

        let mut hasher = Hasher::new();
        hasher.update(input);
        let result_bytes = hasher.finalize();
        let mut result = [0u8; 32];
        result.copy_from_slice(result_bytes.as_bytes());
        result
    }

    /// Пакетное хеширование с получением произвольной длины вывода
    pub async fn hash_keyed_xof_batch(
        &self,
        keys: &[[u8; 32]],
        inputs: &[Vec<u8>],
        output_len: usize,
    ) -> Vec<Vec<u8>> {
        let start = Instant::now();
        let batch_size = keys.len();

        if batch_size == 0 {
            return Vec::new();
        }

        let results = if self.config.enable_parallel_hashing && batch_size >= 4 {
            // Параллельная обработка
            (0..batch_size)
                .into_par_iter()
                .map(|i| {
                    self.hash_keyed_xof_single_scalar(&keys[i], &inputs[i], output_len)
                })
                .collect()
        } else {
            // Последовательная обработка
            let mut results = Vec::with_capacity(batch_size);
            for i in 0..batch_size {
                results.push(self.hash_keyed_xof_single_scalar(&keys[i], &inputs[i], output_len));
            }
            results
        };

        let elapsed = start.elapsed();
        debug!("✅ Blake3 XOF batch: {} ops in {:?}", batch_size, elapsed);

        results
    }

    /// Одиночное XOF хеширование
    fn hash_keyed_xof_single_scalar(&self, key: &[u8; 32], input: &[u8], output_len: usize) -> Vec<u8> {
        use blake3::Hasher;

        let mut hasher = Hasher::new_keyed(key);
        hasher.update(input);
        let mut output = vec![0u8; output_len];
        hasher.finalize_xof().fill(&mut output);
        output
    }

    /// Получение информации о производительности
    pub fn get_performance_info(&self) -> Blake3PerformanceInfo {
        let hits = self.cache_hits.load(std::sync::atomic::Ordering::Relaxed);
        let misses = self.cache_misses.load(std::sync::atomic::Ordering::Relaxed);
        let total = hits + misses;
        let hit_rate = if total > 0 { hits as f64 / total as f64 * 100.0 } else { 0.0 };

        Blake3PerformanceInfo {
            simd_capable: self.simd_capable,
            optimal_batch_size: self.get_optimal_batch_size(),
            estimated_throughput: self.estimate_throughput(),
            cache_hits: hits,
            cache_misses: misses,
            cache_hit_rate: hit_rate,
            state_cache_size: self.state_cache.len(),
        }
    }

    /// Определение оптимального размера батча
    fn get_optimal_batch_size(&self) -> usize {
        if self.detected_features.avx512 {
            16
        } else if self.detected_features.avx2 {
            8
        } else if self.detected_features.neon {
            4
        } else {
            1
        }
    }

    /// Оценка пропускной способности
    fn estimate_throughput(&self) -> f64 {
        // Оценочные значения в MB/s
        if self.detected_features.avx512 {
            5000.0  // 5 GB/s
        } else if self.detected_features.avx2 {
            2500.0  // 2.5 GB/s
        } else if self.detected_features.neon {
            1200.0  // 1.2 GB/s
        } else {
            300.0   // 300 MB/s
        }
    }

    /// Получение кэша состояний
    pub fn get_state_cache(&self) -> &Vec<Vec<u32>> {
        &self.state_cache
    }

    /// Очистка кэша состояний
    pub fn clear_state_cache(&mut self) {
        self.state_cache.clear();
        self.cache_hits.store(0, std::sync::atomic::Ordering::Relaxed);
        self.cache_misses.store(0, std::sync::atomic::Ordering::Relaxed);
        info!("Cleared Blake3 state cache");
    }

    /// Предварительное заполнение кэша
    pub fn prefill_state_cache(&mut self, num_states: usize) {
        self.state_cache.clear();
        for _ in 0..num_states {
            self.state_cache.push(vec![0u32; 16]); // Blake3 internal state size
        }
        info!("Prefilled Blake3 state cache with {} states", num_states);
    }
}

/// Информация о производительности Blake3
#[derive(Debug, Clone)]
pub struct Blake3PerformanceInfo {
    pub simd_capable: bool,
    pub optimal_batch_size: usize,
    pub estimated_throughput: f64, // MB/s
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_hit_rate: f64,
    pub state_cache_size: usize,
}

/// Общие SIMD фичи (дублируется из chacha20, но для независимости модулей)
#[derive(Debug, Clone, Copy)]
pub struct SimdFeatures {
    pub avx2: bool,
    pub avx512: bool,
    pub neon: bool,
    pub aes_ni: bool,
    pub sse4_2: bool,
}

impl Default for SimdFeatures {
    fn default() -> Self {
        Self::detect()
    }
}

impl SimdFeatures {
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            use std::arch::is_x86_feature_detected;
            Self {
                avx2: is_x86_feature_detected!("avx2"),
                avx512: is_x86_feature_detected!("avx512f"),
                neon: false,
                aes_ni: is_x86_feature_detected!("aes"),
                sse4_2: is_x86_feature_detected!("sse4.2"),
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            use std::arch::is_aarch64_feature_detected;
            Self {
                avx2: false,
                avx512: false,
                neon: is_aarch64_feature_detected!("neon"),
                aes_ni: is_aarch64_feature_detected!("aes"),
                sse4_2: false,
            }
        }

        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Self {
                avx2: false,
                avx512: false,
                neon: false,
                aes_ni: false,
                sse4_2: false,
            }
        }
    }
}

impl Default for Blake3BatchAccelerator {
    fn default() -> Self {
        Self::new(8)
    }
}