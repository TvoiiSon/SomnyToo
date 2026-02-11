use std::time::Instant;
use tracing::{info, debug};
use rayon::prelude::*;

/// Конфигурация SIMD ускорителя Blake3
#[derive(Debug, Clone)]
pub struct Blake3BatchConfig {
    pub simd_width: usize,
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
}

/// SIMD фичи
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

impl Blake3BatchAccelerator {
    pub fn new(simd_width: usize) -> Self {
        let config = Blake3BatchConfig {
            simd_width,
            ..Default::default()
        };

        let detected_features = SimdFeatures::detect();
        let simd_capable = detected_features.avx2 || detected_features.neon;

        info!("🚀 Blake3BatchAccelerator initialized:");
        info!("  - SIMD width: {}", simd_width);
        info!("  - AVX2: {}", detected_features.avx2);
        info!("  - AVX512: {}", detected_features.avx512);
        info!("  - NEON: {}", detected_features.neon);
        info!("  - AES-NI: {}", detected_features.aes_ni);
        info!("  - SSE4.2: {}", detected_features.sse4_2);
        info!("  - SIMD capable: {}", simd_capable);

        let accelerator = Self {
            config,
            simd_capable,
            detected_features,
        };

        accelerator
    }

    /// Пакетное хеширование с ключом
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

        let results = if self.config.enable_parallel_hashing && batch_size >= 4 {
            self.hash_keyed_batch_parallel(keys, inputs)
        } else {
            self.hash_keyed_batch_scalar(keys, inputs)
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
        let batch_size = inputs.len();
        let zero_keys = vec![[0u8; 32]; batch_size];
        self.hash_keyed_batch(&zero_keys, inputs).await
    }

    /// Параллельное хеширование
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

    /// Скалярное хеширование
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

    /// Одиночное хеширование с ключом
    fn hash_keyed_single(&self, key: &[u8; 32], input: &[u8]) -> [u8; 32] {
        use blake3::Hasher;

        let mut hasher = Hasher::new_keyed(key);
        hasher.update(input);
        let result_bytes = hasher.finalize();
        let mut result = [0u8; 32];
        result.copy_from_slice(result_bytes.as_bytes());
        result
    }

    /// Получение информации о производительности
    pub fn get_performance_info(&self) -> Blake3PerformanceInfo {
        // Используем detected_features для более точной оценки производительности
        let base_throughput = if self.detected_features.avx512 {
            2048.0  // AVX512 дает максимальную производительность
        } else if self.detected_features.avx2 {
            1024.0  // AVX2 - хорошая производительность
        } else if self.detected_features.neon {
            896.0   // NEON - хорошая производительность на ARM
        } else if self.detected_features.sse4_2 {
            512.0   // SSE4.2 - базовая SIMD
        } else {
            256.0   // Без SIMD
        };

        // Учитываем AES-NI для ключевого хеширования
        let aes_bonus = if self.detected_features.aes_ni { 1.2 } else { 1.0 };

        Blake3PerformanceInfo {
            simd_capable: self.simd_capable,
            optimal_batch_size: self.config.simd_width * 4,
            estimated_throughput: base_throughput * aes_bonus,
            avx512_enabled: self.detected_features.avx512,  // Новое поле
            avx2_enabled: self.detected_features.avx2,      // Новое поле
            neon_enabled: self.detected_features.neon,      // Новое поле
            aes_ni_enabled: self.detected_features.aes_ni,  // Новое поле
        }
    }
}

/// Информация о производительности Blake3
#[derive(Debug, Clone)]
pub struct Blake3PerformanceInfo {
    pub simd_capable: bool,
    pub optimal_batch_size: usize,
    pub estimated_throughput: f64, // MB/s
    pub avx512_enabled: bool,      // Добавляем поле
    pub avx2_enabled: bool,        // Добавляем поле
    pub neon_enabled: bool,        // Добавляем поле
    pub aes_ni_enabled: bool,      // Добавляем поле
}

impl Default for Blake3BatchAccelerator {
    fn default() -> Self {
        Self::new(8)
    }
}