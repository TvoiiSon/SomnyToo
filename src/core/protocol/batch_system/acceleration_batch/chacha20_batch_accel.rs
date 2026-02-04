use std::time::{Instant};
use rayon::prelude::*;
use tracing::{info, debug};
use zeroize::Zeroize;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

/// Конфигурация SIMD ускорителя ChaCha20
#[derive(Debug, Clone)]
pub struct ChaCha20BatchConfig {
    pub simd_width: usize,           // 4, 8, 16
    pub enable_avx2: bool,
    pub enable_avx512: bool,
    pub enable_neon: bool,
    pub batch_alignment: usize,      // Выравнивание для SIMD
    pub max_concurrent_batches: usize,
    pub enable_parallel: bool,  // Добавлено поле
}

impl Default for ChaCha20BatchConfig {
    fn default() -> Self {
        Self {
            simd_width: 8,
            enable_avx2: true,
            enable_avx512: false,
            enable_neon: true,
            batch_alignment: 64,     // Кэш-линия
            max_concurrent_batches: 8,
            enable_parallel: true,
        }
    }
}

/// SIMD-вектор для ChaCha20
#[repr(align(64))]
#[derive(Debug, Clone)]
pub struct ChaCha20Vector {
    data: Vec<[u8; 64]>,            // Выровненные данные
    capacity: usize,
}

impl ChaCha20Vector {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![[0u8; 64]; capacity],
            capacity,
        }
    }

    // Добавляем метод для использования capacity
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    // Добавляем метод для изменения capacity
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.data.as_mut_ptr() as *mut u8
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.data.as_ptr() as *const u8
    }
}

impl Zeroize for ChaCha20Vector {
    fn zeroize(&mut self) {
        for block in &mut self.data {
            block.zeroize();
        }
    }
}

/// Пакетный акселератор ChaCha20
pub struct ChaCha20BatchAccelerator {
    config: ChaCha20BatchConfig,
    state_cache: Vec<ChaCha20Vector>,   // Кэш состояний
    output_cache: Vec<ChaCha20Vector>,  // Кэш выходных данных
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
            use std::is_x86_feature_detected;
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

        // Предвыделяем кэш
        let state_cache = Vec::with_capacity(config.max_concurrent_batches);
        let output_cache = Vec::with_capacity(config.max_concurrent_batches);

        info!("🚀 ChaCha20BatchAccelerator initialized:");
        info!("  - SIMD width: {}", simd_width);
        info!("  - AVX2: {}", detected_features.avx2);
        info!("  - AVX512: {}", detected_features.avx512);
        info!("  - NEON: {}", detected_features.neon);
        info!("  - SIMD capable: {}", simd_capable);

        Self {
            config,
            state_cache,
            output_cache,
            simd_capable,
            detected_features,
        }
    }

    /// Пакетное шифрование (SIMD оптимизированное)
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

        // Выбираем оптимальную реализацию
        let results = if self.simd_capable && batch_size >= 4 {
            #[cfg(target_arch = "x86_64")]
            {
                if self.detected_features.avx2 && self.config.enable_avx2 && batch_size % 8 == 0 {
                    self.encrypt_batch_avx2(keys, nonces, plaintexts).await
                } else {
                    self.encrypt_batch_simd_fallback(keys, nonces, plaintexts).await
                }
            }

            #[cfg(target_arch = "aarch64")]
            {
                if self.detected_features.neon && self.config.enable_neon {
                    self.encrypt_batch_neon(keys, nonces, plaintexts).await
                } else {
                    self.encrypt_batch_simd_fallback(keys, nonces, plaintexts).await
                }
            }

            #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
            {
                self.encrypt_batch_simd_fallback(keys, nonces, plaintexts).await
            }
        } else {
            self.encrypt_batch_scalar(keys, nonces, plaintexts).await
        };

        let elapsed = start.elapsed();
        debug!("✅ ChaCha20 batch encryption completed in {:?} ({:.1} ops/ms)",
               elapsed, batch_size as f64 / elapsed.as_millis() as f64);

        results
    }

    /// Пакетное дешифрование (аналогично шифрованию для ChaCha20)
    pub async fn decrypt_batch(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        ciphertexts: &[Vec<u8>],
    ) -> Vec<Vec<u8>> {
        // ChaCha20 - симметричный, дешифрование = шифрование
        self.encrypt_batch(keys, nonces, ciphertexts).await
    }

    /// AVX2 оптимизированное шифрование
    #[cfg(target_arch = "x86_64")]
    async fn encrypt_batch_avx2(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        plaintexts: &[Vec<u8>],
    ) -> Vec<Vec<u8>> {
        unsafe {
            use std::arch::x86_64::*;

            let batch_size = keys.len();
            let simd_width = 8; // AVX2 работает с 256-битными регистрами (8 u32)
            let mut results = Vec::with_capacity(batch_size);

            // Обрабатываем по SIMD_WIDTH за раз
            for chunk_start in (0..batch_size).step_by(simd_width) {
                let chunk_end = (chunk_start + simd_width).min(batch_size);
                let chunk_size = chunk_end - chunk_start;

                // Подготавливаем SIMD регистры
                let mut key_regs = [std::mem::MaybeUninit::<__m256i>::uninit(); 8];
                let mut nonce_regs = [std::mem::MaybeUninit::<__m256i>::uninit(); 3];

                for i in 0..chunk_size {
                    // Загружаем ключ (8 x u32)
                    let key = _mm256_loadu_si256(keys[chunk_start + i].as_ptr() as *const __m256i);
                    key_regs[i].write(key);

                    // Загружаем nonce (3 x u32 + counter)
                    let nonce_data = &nonces[chunk_start + i];
                    let nonce_part = _mm256_setr_epi32(
                        u32::from_le_bytes([nonce_data[0], nonce_data[1], nonce_data[2], nonce_data[3]]) as i32,
                        u32::from_le_bytes([nonce_data[4], nonce_data[5], nonce_data[6], nonce_data[7]]) as i32,
                        u32::from_le_bytes([nonce_data[8], nonce_data[9], nonce_data[10], nonce_data[11]]) as i32,
                        1, // counter
                        0, 0, 0, 0
                    );
                    nonce_regs[i].write(nonce_part);
                }

                // Обрабатываем каждый plaintext в чанке
                for i in 0..chunk_size {
                    let idx = chunk_start + i;
                    let plaintext = &plaintexts[idx];
                    let mut ciphertext = vec![0u8; plaintext.len()];

                    // ChaCha20 quarter rounds в AVX2
                    let mut state = [
                        _mm256_set1_epi32(0x61707865), // "expa"
                        _mm256_set1_epi32(0x3320646e), // "nd 3"
                        _mm256_set1_epi32(0x79622d32), // "2-by"
                        _mm256_set1_epi32(0x6b206574), // "te k"
                        key_regs[i].assume_init(),
                        key_regs[i].assume_init(),
                        key_regs[i].assume_init(),
                        key_regs[i].assume_init(),
                        nonce_regs[i].assume_init(),
                        _mm256_setzero_si256(),
                        _mm256_setzero_si256(),
                        _mm256_setzero_si256(),
                        _mm256_setzero_si256(),
                        _mm256_setzero_si256(),
                        _mm256_setzero_si256(),
                        _mm256_setzero_si256(),
                    ];

                    // 20 rounds (10 double rounds) в AVX2
                    for _ in 0..10 {
                        // Column rounds
                        self.quarter_round_avx2(&mut state, 0, 4, 8, 12);
                        self.quarter_round_avx2(&mut state, 1, 5, 9, 13);
                        self.quarter_round_avx2(&mut state, 2, 6, 10, 14);
                        self.quarter_round_avx2(&mut state, 3, 7, 11, 15);

                        // Diagonal rounds
                        self.quarter_round_avx2(&mut state, 0, 5, 10, 15);
                        self.quarter_round_avx2(&mut state, 1, 6, 11, 12);
                        self.quarter_round_avx2(&mut state, 2, 7, 8, 13);
                        self.quarter_round_avx2(&mut state, 3, 4, 9, 14);
                    }

                    // XOR с plaintext
                    let blocks_needed = (plaintext.len() + 63) / 64;
                    for block in 0..blocks_needed {
                        let start = block * 64;
                        let end = (start + 64).min(plaintext.len());

                        // Генерируем keystream
                        let keystream = [0u8; 64];
                        // TODO: Заполнить keystream из state

                        // XOR
                        for j in start..end {
                            ciphertext[j] = plaintext[j] ^ keystream[j - start];
                        }
                    }

                    results.push(ciphertext);
                }
            }

            results
        }
    }

    /// AVX2 quarter round
    #[cfg(target_arch = "x86_64")]
    unsafe fn quarter_round_avx2(&self, state: &mut [__m256i; 16], a: usize, b: usize, c: usize, d: usize) {
        use std::arch::x86_64::*;

        unsafe {
            // a += b
            state[a] = _mm256_add_epi32(state[a], state[b]);
            // d ^= a
            state[d] = _mm256_xor_si256(state[d], state[a]);
            // d <<<= 16
            state[d] = _mm256_or_si256(
                _mm256_slli_epi32(state[d], 16),
                _mm256_srli_epi32(state[d], 16)
            );
            // c += d
            state[c] = _mm256_add_epi32(state[c], state[d]);
            // b ^= c
            state[b] = _mm256_xor_si256(state[b], state[c]);
            // b <<<= 12
            state[b] = _mm256_or_si256(
                _mm256_slli_epi32(state[b], 12),
                _mm256_srli_epi32(state[b], 20)
            );
            // a += b
            state[a] = _mm256_add_epi32(state[a], state[b]);
            // d ^= a
            state[d] = _mm256_xor_si256(state[d], state[a]);
            // d <<<= 8
            state[d] = _mm256_or_si256(
                _mm256_slli_epi32(state[d], 8),
                _mm256_srli_epi32(state[d], 24)
            );
            // c += d
            state[c] = _mm256_add_epi32(state[c], state[d]);
            // b ^= c
            state[b] = _mm256_xor_si256(state[b], state[c]);
            // b <<<= 7
            state[b] = _mm256_or_si256(
                _mm256_slli_epi32(state[b], 7),
                _mm256_srli_epi32(state[b], 25)
            );
        }
    }

    /// NEON оптимизированное шифрование
    #[cfg(target_arch = "aarch64")]
    async fn encrypt_batch_neon(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        plaintexts: &[Vec<u8>],
    ) -> Vec<Vec<u8>> {
        // ARM NEON реализация
        warn!("NEON implementation not yet available, using scalar fallback");
        self.encrypt_batch_scalar(keys, nonces, plaintexts).await
    }

    /// SIMD fallback (универсальная реализация)
    async fn encrypt_batch_simd_fallback(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        plaintexts: &[Vec<u8>],
    ) -> Vec<Vec<u8>> {
        let batch_size = keys.len();

        if self.config.enable_parallel && batch_size >= self.config.simd_width {
            // Параллельная обработка
            (0..batch_size)
                .into_par_iter()
                .map(|i| {
                    self.encrypt_single_scalar(&keys[i], &nonces[i], &plaintexts[i])
                })
                .collect()
        } else {
            // Последовательная обработка
            let mut results = Vec::with_capacity(batch_size);
            for i in 0..batch_size {
                results.push(self.encrypt_single_scalar(&keys[i], &nonces[i], &plaintexts[i]));
            }
            results
        }
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

    /// Шифрование in-place с поддержкой SIMD
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

        if self.simd_capable && batch_size >= 4 {
            // SIMD оптимизированная версия
            if self.detected_features.avx2 && self.config.enable_avx2 {
                #[cfg(target_arch = "x86_64")]
                self.encrypt_in_place_avx2(keys, nonces, buffers).await;
                #[cfg(not(target_arch = "x86_64"))]
                self.encrypt_in_place_scalar(keys, nonces, buffers).await;
            } else if self.detected_features.neon && self.config.enable_neon {
                #[cfg(target_arch = "aarch64")]
                self.encrypt_in_place_neon(keys, nonces, buffers).await;
                #[cfg(not(target_arch = "aarch64"))]
                self.encrypt_in_place_scalar(keys, nonces, buffers).await;
            } else {
                self.encrypt_in_place_scalar(keys, nonces, buffers).await;
            }
        } else {
            self.encrypt_in_place_scalar(keys, nonces, buffers).await;
        }

        let elapsed = start.elapsed();
        debug!("✅ ChaCha20 in-place batch encryption: {} ops in {:?}",
               batch_size, elapsed);
    }

    /// Скалярное in-place шифрование
    async fn encrypt_in_place_scalar(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        buffers: &mut [Vec<u8>],
    ) {
        use chacha20::cipher::{KeyIvInit, StreamCipher};
        use chacha20::ChaCha20;

        let batch_size = keys.len();

        if self.config.enable_parallel && batch_size >= 4 {
            // Параллельная обработка
            buffers.par_iter_mut().enumerate().for_each(|(i, buffer)| {
                let mut cipher = ChaCha20::new((&keys[i]).into(), (&nonces[i]).into());
                cipher.apply_keystream(buffer);
            });
        } else {
            // Последовательная обработка
            for i in 0..batch_size {
                let mut cipher = ChaCha20::new((&keys[i]).into(), (&nonces[i]).into());
                cipher.apply_keystream(&mut buffers[i]);
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    async fn encrypt_in_place_avx2(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        buffers: &mut [Vec<u8>],
    ) {
        // AVX2 оптимизированная версия in-place
        // TODO: Реализовать AVX2 in-place шифрование
        self.encrypt_in_place_scalar(keys, nonces, buffers).await;
    }

    #[cfg(target_arch = "aarch64")]
    async fn encrypt_in_place_neon(
        &self,
        keys: &[[u8; 32]],
        nonces: &[[u8; 12]],
        buffers: &mut [Vec<u8>],
    ) {
        // NEON оптимизированная версия in-place
        self.encrypt_in_place_scalar(keys, nonces, buffers).await;
    }

    /// Получение информации о SIMD возможностях
    pub fn get_simd_info(&self) -> SimdInfo {
        SimdInfo {
            features: self.detected_features,
            simd_capable: self.simd_capable,
            optimal_batch_size: self.get_optimal_batch_size(),
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

    // Добавляем метод для использования state_cache
    pub fn state_cache_size(&self) -> usize {
        self.state_cache.len()
    }

    // Добавляем метод для использования output_cache
    pub fn output_cache_size(&self) -> usize {
        self.output_cache.len()
    }

    // Добавляем метод для очистки кэшей
    pub fn clear_caches(&mut self) {
        self.state_cache.clear();
        self.output_cache.clear();
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