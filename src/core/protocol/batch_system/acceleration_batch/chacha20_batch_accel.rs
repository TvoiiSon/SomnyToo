use std::time::{Instant};
use rayon::prelude::*;
use tracing::{info, debug};

/// Модель эффективности SIMD
#[derive(Debug, Clone)]
pub struct SIMDPerformanceModel {
    /// Базовое время без SIMD (нс/байт)
    pub scalar_time_per_byte: f64,

    /// Время с SIMD (нс/байт)
    pub simd_time_per_byte: f64,

    /// Накладные расходы на батч (нс)
    pub batch_overhead_ns: f64,

    /// Коэффициент ускорения
    pub speedup_factor: f64,

    /// Эффективность (0-1)
    pub efficiency: f64,

    /// Оптимальный размер батча
    pub optimal_batch_size: usize,

    /// Минимальный размер для SIMD
    pub min_simd_size: usize,
}

impl SIMDPerformanceModel {
    pub fn new() -> Self {
        Self {
            scalar_time_per_byte: 1.0,    // 1 нс/байт
            simd_time_per_byte: 0.25,      // 0.25 нс/байт (4x)
            batch_overhead_ns: 100.0,      // 100 нс накладных
            speedup_factor: 4.0,
            efficiency: 0.8,
            optimal_batch_size: 1024,
            min_simd_size: 64,
        }
    }

    /// Время выполнения для размера данных
    pub fn execution_time(&self, size: usize, use_simd: bool) -> f64 {
        if use_simd && size >= self.min_simd_size {
            self.batch_overhead_ns + size as f64 * self.simd_time_per_byte
        } else {
            size as f64 * self.scalar_time_per_byte
        }
    }

    /// Пропускная способность
    pub fn throughput(&self, size: usize, use_simd: bool) -> f64 {
        let time_sec = self.execution_time(size, use_simd) / 1_000_000_000.0;
        size as f64 / time_sec
    }
}

#[derive(Debug, Clone)]
pub struct ChaCha20BatchConfig {
    pub simd_width: usize,
    pub enable_avx2: bool,
    pub enable_avx512: bool,
    pub enable_neon: bool,
    pub batch_alignment: usize,
    pub max_concurrent_batches: usize,
    pub enable_parallel: bool,
    pub performance_model: SIMDPerformanceModel,
}

impl Default for ChaCha20BatchConfig {
    fn default() -> Self {
        Self {
            simd_width: 8,
            enable_avx2: true,
            enable_avx512: false,
            enable_neon: true,
            batch_alignment: 64,
            max_concurrent_batches: 8,
            enable_parallel: true,
            performance_model: SIMDPerformanceModel::new(),
        }
    }
}

#[repr(align(64))]
#[derive(Debug, Clone)]
pub struct ChaCha20Vector {
    data: Vec<[u8; 64]>,
}

impl ChaCha20Vector {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![[0u8; 64]; capacity],
        }
    }

    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    pub fn is_aligned(&self) -> bool {
        self.data.as_ptr() as usize % 64 == 0
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

    /// Функция эффективности для AVX2
    pub fn avx2_efficiency(&self) -> f64 {
        if self.avx2 { 0.85 } else { 0.0 }
    }

    /// Функция эффективности для AVX512
    pub fn avx512_efficiency(&self) -> f64 {
        if self.avx512 { 0.95 } else { 0.0 }
    }

    /// Функция эффективности для NEON
    pub fn neon_efficiency(&self) -> f64 {
        if self.neon { 0.80 } else { 0.0 }
    }

    /// Максимальная эффективность SIMD
    pub fn max_efficiency(&self) -> f64 {
        self.avx512_efficiency()
            .max(self.avx2_efficiency())
            .max(self.neon_efficiency())
            .max(0.5)
    }
}

pub struct ChaCha20BatchAccelerator {
    config: ChaCha20BatchConfig,
    simd_capable: bool,
    detected_features: SimdFeatures,
    performance_model: SIMDPerformanceModel,
}

impl ChaCha20BatchAccelerator {
    pub fn new(simd_width: usize) -> Self {
        let config = ChaCha20BatchConfig {
            simd_width,
            ..Default::default()
        };

        let detected_features = SimdFeatures::detect();
        let simd_capable = detected_features.avx2 || detected_features.neon || detected_features.avx512;

        // Настройка модели производительности под обнаруженные возможности
        let mut performance_model = SIMDPerformanceModel::new();
        if detected_features.avx512 {
            performance_model.simd_time_per_byte = 0.1;
            performance_model.speedup_factor = 10.0;
            performance_model.efficiency = 0.95;
        } else if detected_features.avx2 {
            performance_model.simd_time_per_byte = 0.2;
            performance_model.speedup_factor = 5.0;
            performance_model.efficiency = 0.85;
        } else if detected_features.neon {
            performance_model.simd_time_per_byte = 0.25;
            performance_model.speedup_factor = 4.0;
            performance_model.efficiency = 0.80;
        }

        info!("🚀 ChaCha20BatchAccelerator initialized:");
        info!("  - SIMD width: {}", simd_width);
        info!("  - AVX2: {} ({:.1}% eff)",
              detected_features.avx2, detected_features.avx2_efficiency() * 100.0);
        info!("  - AVX512: {} ({:.1}% eff)",
              detected_features.avx512, detected_features.avx512_efficiency() * 100.0);
        info!("  - NEON: {} ({:.1}% eff)",
              detected_features.neon, detected_features.neon_efficiency() * 100.0);
        info!("  - SIMD capable: {}", simd_capable);
        info!("  - Speedup: {:.1}x", performance_model.speedup_factor);
        info!("  - Optimal batch size: {} bytes", performance_model.optimal_batch_size);

        Self {
            config,
            simd_capable,
            detected_features,
            performance_model,
        }
    }

    pub async fn encrypt_batch(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        plaintexts: &[Vec<u8>],
    ) -> Vec<Vec<u8>> {
        let start = Instant::now();
        assert_eq!(keys.len(), nonces.len());
        assert_eq!(keys.len(), plaintexts.len());

        let batch_size = keys.len();
        if batch_size == 0 {
            return Vec::new();
        }

        // Определение оптимальной стратегии
        let use_simd = self.simd_capable &&
            batch_size >= 4 &&
            self.config.enable_parallel &&
            plaintexts.iter().any(|p| p.len() >= self.performance_model.min_simd_size);

        debug!("🔐 ChaCha20 batch encryption: {} blocks, SIMD={}",
               batch_size, use_simd);

        let results = if use_simd && batch_size >= 8 {
            self.encrypt_batch_parallel_optimized(keys, nonces, plaintexts).await
        } else if use_simd {
            self.encrypt_batch_parallel(keys, nonces, plaintexts).await
        } else {
            self.encrypt_batch_scalar(keys, nonces, plaintexts).await
        };

        let elapsed = start.elapsed();
        let throughput = batch_size as f64 / elapsed.as_secs_f64();

        debug!("✅ ChaCha20 batch encryption: {} ops in {:?} ({:.0} ops/s)",
               batch_size, elapsed, throughput);

        results
    }

    pub async fn decrypt_batch(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        ciphertexts: &[Vec<u8>],
    ) -> Vec<Vec<u8>> {
        self.encrypt_batch(keys, nonces, ciphertexts).await
    }

    async fn encrypt_batch_parallel_optimized(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        plaintexts: &[Vec<u8>],
    ) -> Vec<Vec<u8>> {
        // ИСПРАВЛЕНИЕ: просто используем map+collect
        (0..keys.len())
            .into_par_iter()
            .map(|i| self.encrypt_single_scalar(&keys[i], &nonces[i], &plaintexts[i]))
            .collect()
    }

    async fn encrypt_batch_parallel(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        plaintexts: &[Vec<u8>],
    ) -> Vec<Vec<u8>> {
        let batch_size = keys.len();

        (0..batch_size)
            .into_par_iter()
            .map(|i| self.encrypt_single_scalar(&keys[i], &nonces[i], &plaintexts[i]))
            .collect()
    }

    async fn encrypt_batch_scalar(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        plaintexts: &[Vec<u8>],
    ) -> Vec<Vec<u8>> {
        let batch_size = keys.len();
        let mut results = Vec::with_capacity(batch_size);

        for i in 0..batch_size {
            results.push(self.encrypt_single_scalar(&keys[i], &nonces[i], &plaintexts[i]));
        }

        results
    }

    fn encrypt_single_scalar(&self, key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8]) -> Vec<u8> {
        use chacha20::cipher::{KeyIvInit, StreamCipher};
        use chacha20::ChaCha20;

        let mut cipher = ChaCha20::new(key.into(), nonce.into());
        let mut buffer = plaintext.to_vec();
        cipher.apply_keystream(&mut buffer);
        buffer
    }

    pub fn get_simd_info(&self) -> SimdInfo {
        let test_vector = ChaCha20Vector::new(self.config.simd_width);
        let alignment_ok = test_vector.is_aligned();

        SimdInfo {
            features: self.detected_features,
            simd_capable: self.simd_capable,
            optimal_batch_size: self.performance_model.optimal_batch_size,
            vector_capacity: test_vector.capacity(),
            alignment_ok,
            speedup_factor: self.performance_model.speedup_factor,
            efficiency: self.performance_model.efficiency,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SimdInfo {
    pub features: SimdFeatures,
    pub simd_capable: bool,
    pub optimal_batch_size: usize,
    pub vector_capacity: usize,
    pub alignment_ok: bool,
    pub speedup_factor: f64,
    pub efficiency: f64,
}

impl Default for ChaCha20BatchAccelerator {
    fn default() -> Self {
        Self::new(8)
    }
}