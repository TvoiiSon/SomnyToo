use std::time::Instant;
use rayon::prelude::*;
use tracing::{info, debug};

#[cfg(target_arch = "x86_64")]

/// Конфигурация SIMD ускорителя ChaCha20
#[derive(Debug, Clone)]
pub struct ChaCha20BatchConfig {
    pub simd_width: usize,
    pub enable_avx2: bool,
    pub enable_avx512: bool,
    pub enable_neon: bool,
    pub batch_alignment: usize,
    pub max_concurrent_batches: usize,
    pub enable_parallel: bool,
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
        }
    }
}

/// SIMD-вектор для ChaCha20 (упрощенно)
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
}

/// Пакетный акселератор ChaCha20
pub struct ChaCha20BatchAccelerator {
    config: ChaCha20BatchConfig,
    simd_capable: bool,
    detected_features: SimdFeatures,
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
}

impl ChaCha20BatchAccelerator {
    pub fn new(simd_width: usize) -> Self {
        let config = ChaCha20BatchConfig {
            simd_width,
            ..Default::default()
        };

        let detected_features = SimdFeatures::detect();
        let simd_capable = detected_features.avx2 || detected_features.neon;

        info!("🚀 ChaCha20BatchAccelerator initialized:");
        info!("  - SIMD width: {}", simd_width);
        info!("  - AVX2: {}", detected_features.avx2);
        info!("  - AVX512: {}", detected_features.avx512);
        info!("  - NEON: {}", detected_features.neon);
        info!("  - SIMD capable: {}", simd_capable);

        Self {
            config,
            simd_capable,
            detected_features,
        }
    }

    /// Пакетное шифрование
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

        debug!("🔐 ChaCha20 batch encryption: {} blocks", batch_size);

        let results = if self.simd_capable && batch_size >= 4 && self.config.enable_parallel {
            self.encrypt_batch_parallel(keys, nonces, plaintexts).await
        } else {
            self.encrypt_batch_scalar(keys, nonces, plaintexts).await
        };

        let elapsed = start.elapsed();
        debug!("✅ ChaCha20 batch encryption completed in {:?} ({:.1} ops/ms)",
               elapsed, batch_size as f64 / elapsed.as_millis() as f64);

        results
    }

    /// Пакетное дешифрование
    pub async fn decrypt_batch(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        ciphertexts: &[Vec<u8>],
    ) -> Vec<Vec<u8>> {
        self.encrypt_batch(keys, nonces, ciphertexts).await
    }

    /// Параллельное шифрование
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

    /// Скалярное шифрование
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

    /// Одиночное скалярное шифрование
    fn encrypt_single_scalar(&self, key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8]) -> Vec<u8> {
        use chacha20::cipher::{KeyIvInit, StreamCipher};
        use chacha20::ChaCha20;

        let mut cipher = ChaCha20::new(key.into(), nonce.into());
        let mut buffer = plaintext.to_vec();
        cipher.apply_keystream(&mut buffer);
        buffer
    }

    /// In-place шифрование
    pub async fn encrypt_in_place_batch(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        buffers: &mut [Vec<u8>],
    ) {
        let start = Instant::now();
        let batch_size = keys.len();

        assert_eq!(batch_size, nonces.len());
        assert_eq!(batch_size, buffers.len());

        if batch_size == 0 {
            return;
        }

        if self.config.enable_parallel && batch_size >= 4 {
            buffers.par_iter_mut().enumerate().for_each(|(i, buffer)| {
                self.encrypt_in_place_single(&keys[i], &nonces[i], buffer);
            });
        } else {
            for i in 0..batch_size {
                self.encrypt_in_place_single(&keys[i], &nonces[i], &mut buffers[i]);
            }
        }

        let elapsed = start.elapsed();
        debug!("✅ ChaCha20 in-place batch encryption: {} ops in {:?}",
               batch_size, elapsed);
    }

    fn encrypt_in_place_single(&self, key: &[u8; 32], nonce: &[u8; 12], buffer: &mut Vec<u8>) {
        use chacha20::cipher::{KeyIvInit, StreamCipher};
        use chacha20::ChaCha20;

        let mut cipher = ChaCha20::new(key.into(), nonce.into());
        cipher.apply_keystream(buffer);
    }

    /// Получение информации о SIMD возможностях
    pub fn get_simd_info(&self) -> SimdInfo {
        SimdInfo {
            features: self.detected_features,
            simd_capable: self.simd_capable,
            optimal_batch_size: self.config.simd_width * 4,
        }
    }
}

/// Информация о SIMD возможностях
#[derive(Debug, Clone)]
pub struct SimdInfo {
    pub features: SimdFeatures,
    pub simd_capable: bool,
    pub optimal_batch_size: usize,
}

impl Default for ChaCha20BatchAccelerator {
    fn default() -> Self {
        Self::new(8)
    }
}