use std::time::{Instant};
use rayon::prelude::*;
use tracing::{info, debug};

/// Степенной закон зависимости времени от размера: T(n) = a * n^b
#[derive(Debug, Clone)]
pub struct PowerLawModel {
    /// Коэффициент масштаба (a)
    pub scale: f64,

    /// Показатель степени (b)
    pub exponent: f64,

    /// Базовое время для малых данных
    pub base_time_ns: f64,

    /// Коэффициент качества (R²)
    pub quality: f64,
}

impl PowerLawModel {
    pub fn new() -> Self {
        Self {
            scale: 10.0,      // 10 нс
            exponent: 1.05,   // почти линейный
            base_time_ns: 50.0, // 50 нс базовых
            quality: 0.95,
        }
    }

    /// Время выполнения для размера n
    pub fn execution_time(&self, n: usize) -> f64 {
        self.base_time_ns + self.scale * (n as f64).powf(self.exponent)
    }
}

#[derive(Debug, Clone)]
pub struct CacheEffectModel {
    /// Размер L1 кэша
    pub l1_cache_size: usize,

    /// Размер L2 кэша
    pub l2_cache_size: usize,

    /// Размер L3 кэша
    pub l3_cache_size: usize,

    /// Штраф за промах L1 (нс)
    pub l1_miss_penalty: f64,

    /// Штраф за промах L2 (нс)
    pub l2_miss_penalty: f64,

    /// Штраф за промах L3 (нс)
    pub l3_miss_penalty: f64,

    /// Размер строки кэша
    pub cache_line_size: usize,
}

impl CacheEffectModel {
    pub fn new() -> Self {
        Self {
            l1_cache_size: 32 * 1024,      // 32 KB
            l2_cache_size: 256 * 1024,     // 256 KB
            l3_cache_size: 8 * 1024 * 1024, // 8 MB
            l1_miss_penalty: 10.0,          // 10 нс
            l2_miss_penalty: 30.0,          // 30 нс
            l3_miss_penalty: 100.0,         // 100 нс
            cache_line_size: 64,            // 64 байта
        }
    }

    /// Количество промахов кэша для размера данных
    pub fn cache_misses(&self, size: usize) -> usize {
        let lines = (size + self.cache_line_size - 1) / self.cache_line_size;

        if size <= self.l1_cache_size {
            0
        } else if size <= self.l2_cache_size {
            lines - self.l1_cache_size / self.cache_line_size
        } else if size <= self.l3_cache_size {
            lines - self.l2_cache_size / self.cache_line_size
        } else {
            lines - self.l3_cache_size / self.cache_line_size
        }
    }

    /// Штраф за кэш-промахи
    pub fn cache_penalty(&self, size: usize) -> f64 {
        let misses = self.cache_misses(size);

        if size <= self.l1_cache_size {
            0.0
        } else if size <= self.l2_cache_size {
            misses as f64 * self.l1_miss_penalty
        } else if size <= self.l3_cache_size {
            misses as f64 * self.l2_miss_penalty
        } else {
            misses as f64 * self.l3_miss_penalty
        }
    }

    /// Оптимальный размер для кэша
    pub fn optimal_size(&self) -> usize {
        self.l1_cache_size / 2
    }
}

#[derive(Debug, Clone)]
pub struct Blake3BatchConfig {
    pub simd_width: usize,
    pub enable_avx2: bool,
    pub enable_avx512: bool,
    pub enable_neon: bool,
    pub enable_parallel_hashing: bool,
    pub batch_alignment: usize,
    pub max_concurrent_batches: usize,
    pub power_law_model: PowerLawModel,
    pub cache_model: CacheEffectModel,
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
            power_law_model: PowerLawModel::new(),
            cache_model: CacheEffectModel::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SimdFeatures {
    pub avx2: bool,
    pub avx512: bool,
    pub neon: bool,
    pub aes_ni: bool,
    pub sse4_2: bool,
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

    /// Функция эффективности для хеширования
    pub fn hashing_efficiency(&self) -> f64 {
        if self.avx512 {
            0.90
        } else if self.avx2 {
            0.75
        } else if self.neon {
            0.70
        } else {
            0.50
        }
    }
}

pub struct Blake3BatchAccelerator {
    config: Blake3BatchConfig,
    simd_capable: bool,
    detected_features: SimdFeatures,
}

impl Blake3BatchAccelerator {
    pub fn new(simd_width: usize) -> Self {
        let config = Blake3BatchConfig {
            simd_width,
            ..Default::default()
        };

        let detected_features = SimdFeatures::detect();
        let simd_capable = detected_features.avx2 || detected_features.neon || detected_features.avx512;

        let cache_optimal = config.cache_model.optimal_size();
        let _power_law_time = config.power_law_model.execution_time(cache_optimal);

        info!("🚀 Blake3BatchAccelerator initialized:");
        info!("  - SIMD width: {}", simd_width);
        info!("  - AVX2: {}", detected_features.avx2);
        info!("  - AVX512: {}", detected_features.avx512);
        info!("  - NEON: {}", detected_features.neon);
        info!("  - SIMD capable: {}", simd_capable);
        info!("  - Hashing efficiency: {:.1}%", detected_features.hashing_efficiency() * 100.0);
        info!("  - Cache optimal size: {} KB", cache_optimal / 1024);
        info!("  - Power law: T(n) = {:.1} * n^{:.2} ns",
              config.power_law_model.scale, config.power_law_model.exponent);

        Self {
            config,
            simd_capable,
            detected_features,
        }
    }

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

        // Определение оптимальной стратегии
        let use_parallel = self.config.enable_parallel_hashing && batch_size >= 4;
        let avg_size = inputs.iter().map(|i| i.len()).sum::<usize>() / batch_size;
        let use_simd = self.simd_capable && avg_size >= 64;

        debug!("🔑 Blake3 keyed batch: {} blocks, SIMD={}, parallel={}",
               batch_size, use_simd, use_parallel);

        let results = if use_parallel && batch_size >= 16 {
            self.hash_keyed_batch_parallel_optimized(keys, inputs)
        } else if use_parallel {
            self.hash_keyed_batch_parallel(keys, inputs)
        } else {
            self.hash_keyed_batch_scalar(keys, inputs)
        };

        let elapsed = start.elapsed();
        let throughput = batch_size as f64 / elapsed.as_secs_f64();
        let bytes_per_sec = inputs.iter().map(|i| i.len()).sum::<usize>() as f64 / elapsed.as_secs_f64();

        debug!("✅ Blake3 batch hashing: {} ops in {:?} ({:.0} ops/s, {:.1} MB/s)",
               batch_size, elapsed, throughput, bytes_per_sec / 1024.0 / 1024.0);

        results
    }

    fn hash_keyed_batch_parallel_optimized(
        &self,
        keys: &[[u8; 32]],
        inputs: &[Vec<u8>],
    ) -> Vec<[u8; 32]> {
        let batch_size = keys.len();

        // Сортируем индексы
        let mut indices: Vec<usize> = (0..batch_size).collect();
        indices.par_sort_by(|&a, &b| inputs[a].len().cmp(&inputs[b].len()));

        // ИСПРАВЛЕНИЕ: используем par_iter() вместо into_par_iter()
        let sorted_results: Vec<[u8; 32]> = indices
            .par_iter()
            .map(|&i| self.hash_keyed_single(&keys[i], &inputs[i]))
            .collect();

        // Восстанавливаем исходный порядок
        let mut results = vec![[0u8; 32]; batch_size];
        for (pos, &idx) in indices.iter().enumerate() {
            results[idx] = sorted_results[pos];
        }

        results
    }

    fn hash_keyed_batch_parallel(
        &self,
        keys: &[[u8; 32]],
        inputs: &[Vec<u8>],
    ) -> Vec<[u8; 32]> {
        keys.par_iter()
            .zip(inputs.par_iter())
            .map(|(key, input)| self.hash_keyed_single(key, input))
            .collect()
    }

    fn hash_keyed_batch_scalar(
        &self,
        keys: &[[u8; 32]],
        inputs: &[Vec<u8>],
    ) -> Vec<[u8; 32]> {
        let batch_size = keys.len();
        let mut results = Vec::with_capacity(batch_size);

        for i in 0..batch_size {
            results.push(self.hash_keyed_single(&keys[i], &inputs[i]));
        }

        results
    }

    fn hash_keyed_single(&self, key: &[u8; 32], input: &[u8]) -> [u8; 32] {
        use blake3::Hasher;

        let mut hasher = Hasher::new_keyed(key);
        hasher.update(input);
        let result_bytes = hasher.finalize();
        let mut result = [0u8; 32];
        result.copy_from_slice(result_bytes.as_bytes());
        result
    }

    pub fn get_performance_info(&self) -> Blake3PerformanceInfo {
        let base_throughput = if self.detected_features.avx512 {
            2048.0
        } else if self.detected_features.avx2 {
            1024.0
        } else if self.detected_features.neon {
            896.0
        } else {
            512.0
        };

        let aes_bonus = if self.detected_features.aes_ni { 1.2 } else { 1.0 };

        Blake3PerformanceInfo {
            simd_capable: self.simd_capable,
            optimal_batch_size: self.config.simd_width * 4,
            estimated_throughput: base_throughput * aes_bonus,
            avx512_enabled: self.detected_features.avx512,
            avx2_enabled: self.detected_features.avx2,
            neon_enabled: self.detected_features.neon,
            aes_ni_enabled: self.detected_features.aes_ni,
            power_law_exponent: self.config.power_law_model.exponent,
            cache_optimal_size: self.config.cache_model.optimal_size(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ComparisonMetrics {
    pub blake3_time_ns: f64,
    pub chacha20_time_ns: f64,
    pub ratio: f64,
    pub faster: bool,
}

#[derive(Debug, Clone)]
pub struct Blake3PerformanceInfo {
    pub simd_capable: bool,
    pub optimal_batch_size: usize,
    pub estimated_throughput: f64,
    pub avx512_enabled: bool,
    pub avx2_enabled: bool,
    pub neon_enabled: bool,
    pub aes_ni_enabled: bool,
    pub power_law_exponent: f64,
    pub cache_optimal_size: usize,
}

impl Default for Blake3BatchAccelerator {
    fn default() -> Self {
        Self::new(8)
    }
}