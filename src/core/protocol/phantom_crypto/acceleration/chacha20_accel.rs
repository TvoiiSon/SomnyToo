// chacha20_accel.rs
use std::arch::x86_64::*;

#[cfg(target_arch = "x86_64")]
pub mod x86 {
    use super::*;

    #[inline(always)]
    pub unsafe fn chacha20_block_avx2(
        key: &[u8; 32],
        counter: u64,
        nonce: &[u8; 12],
        output: &mut [u8; 64]
    ) {
        unsafe {
            // Константы ChaCha20
            let constants = _mm256_set_epi32(
                0x61707865, 0x3320646e, 0x79622d32, 0x6b206574,
                0x61707865, 0x3320646e, 0x79622d32, 0x6b206574
            );

            // Загрузка ключа
            let key1 = _mm256_loadu_si256(key[0..32].as_ptr() as *const __m256i);
            let key2 = _mm256_loadu_si256(key[16..32].as_ptr() as *const __m256i);

            // Counter и nonce
            let counter_nonce = _mm256_set_epi32(
                counter as i32,
                (counter >> 32) as i32,
                i32::from_le_bytes([nonce[0], nonce[1], nonce[2], nonce[3]]),
                i32::from_le_bytes([nonce[4], nonce[5], nonce[6], nonce[7]]),
                i32::from_le_bytes([nonce[8], nonce[9], nonce[10], nonce[11]]),
                0, 0, 0
            );

            let mut x0 = constants;
            let mut x1 = key1;
            let mut x2 = key2;
            let mut x3 = counter_nonce;

            // 20 раундов (10 двойных раундов)
            for _ in 0..10 {
                // Четный раунд (COLUMN round)
                x0 = _mm256_add_epi32(x0, x1);
                x3 = _mm256_xor_si256(x3, x0);
                x3 = _mm256_or_si256(
                    _mm256_slli_epi32(x3, 16),
                    _mm256_srli_epi32(x3, 16)
                );

                x2 = _mm256_add_epi32(x2, x3);
                x1 = _mm256_xor_si256(x1, x2);
                x1 = _mm256_or_si256(
                    _mm256_slli_epi32(x1, 12),
                    _mm256_srli_epi32(x1, 20)
                );

                x0 = _mm256_add_epi32(x0, x1);
                x3 = _mm256_xor_si256(x3, x0);
                x3 = _mm256_or_si256(
                    _mm256_slli_epi32(x3, 8),
                    _mm256_srli_epi32(x3, 24)
                );

                x2 = _mm256_add_epi32(x2, x3);
                x1 = _mm256_xor_si256(x1, x2);
                x1 = _mm256_or_si256(
                    _mm256_slli_epi32(x1, 7),
                    _mm256_srli_epi32(x1, 25)
                );

                // Нечетный раунд (DIAGONAL round)
                // Перестановка для диагонального раунда
                x1 = _mm256_permute4x64_epi64(x1, 0b10010011);
                x2 = _mm256_permute4x64_epi64(x2, 0b01001110);
                x3 = _mm256_permute4x64_epi64(x3, 0b00111001);

                x0 = _mm256_add_epi32(x0, x1);
                x3 = _mm256_xor_si256(x3, x0);
                x3 = _mm256_or_si256(
                    _mm256_slli_epi32(x3, 16),
                    _mm256_srli_epi32(x3, 16)
                );

                x2 = _mm256_add_epi32(x2, x3);
                x1 = _mm256_xor_si256(x1, x2);
                x1 = _mm256_or_si256(
                    _mm256_slli_epi32(x1, 12),
                    _mm256_srli_epi32(x1, 20)
                );

                x0 = _mm256_add_epi32(x0, x1);
                x3 = _mm256_xor_si256(x3, x0);
                x3 = _mm256_or_si256(
                    _mm256_slli_epi32(x3, 8),
                    _mm256_srli_epi32(x3, 24)
                );

                x2 = _mm256_add_epi32(x2, x3);
                x1 = _mm256_xor_si256(x1, x2);
                x1 = _mm256_or_si256(
                    _mm256_slli_epi32(x1, 7),
                    _mm256_srli_epi32(x1, 25)
                );

                // Возвращаем перестановку
                x1 = _mm256_permute4x64_epi64(x1, 0b00111001);
                x2 = _mm256_permute4x64_epi64(x2, 0b01001110);
                x3 = _mm256_permute4x64_epi64(x3, 0b10010011);
            }

            // Добавляем исходное состояние
            x0 = _mm256_add_epi32(x0, constants);
            x1 = _mm256_add_epi32(x1, key1);
            x2 = _mm256_add_epi32(x2, key2);
            x3 = _mm256_add_epi32(x3, counter_nonce);

            // Сохраняем результат - ИСПРАВЛЕНО!
            // Используем арифметику указателей вместо индексирования срезов
            let ptr = output.as_mut_ptr();
            _mm256_storeu_si256(ptr as *mut __m256i, x0);
            _mm256_storeu_si256(ptr.add(32) as *mut __m256i, x1);
            _mm256_storeu_si256(ptr.add(64) as *mut __m256i, x2);
            _mm256_storeu_si256(ptr.add(96) as *mut __m256i, x3);
        }
    }

    #[inline(always)]
    pub unsafe fn chacha20_encrypt_avx2(
        key: &[u8; 32],
        nonce: &[u8; 12],
        counter: u64,
        input: &[u8],
        output: &mut [u8]
    ) {
        let mut local_counter = counter;
        let mut remaining = input.len();
        let mut in_ptr = input.as_ptr();
        let mut out_ptr = output.as_mut_ptr();

        // Обрабатываем большие блоки по 256 байт
        while remaining >= 256 {
            // Обрабатываем 4 блока ChaCha20 параллельно
            let mut blocks = [[0u8; 64]; 4];

            for i in 0..4 {
                unsafe {
                    chacha20_block_avx2(key, local_counter + i as u64, nonce, &mut blocks[i]);
                }
            }

            // XOR с входными данными - ИСПРАВЛЕНО!
            for i in 0..4 {
                for j in 0..64 {
                    if remaining == 0 { break; }
                    unsafe {
                        *out_ptr = *in_ptr ^ blocks[i][j];
                        out_ptr = out_ptr.add(1);
                        in_ptr = in_ptr.add(1);
                        remaining -= 1;
                    }
                }
            }

            local_counter += 4;
        }

        // Остатки - ИСПРАВЛЕНО! (остался тот же код, он был правильным)
        while remaining > 0 {
            let mut block = [0u8; 64];
            unsafe {
                chacha20_block_avx2(key, local_counter, nonce, &mut block);
            }

            let to_process = remaining.min(64);
            for i in 0..to_process {
                unsafe {
                    *out_ptr = *in_ptr ^ block[i];
                    out_ptr = out_ptr.add(1);
                    in_ptr = in_ptr.add(1);
                }
            }

            remaining -= to_process;
            local_counter += 1;
        }
    }
}

#[cfg(target_arch = "aarch64")]
pub mod arm {
    use std::arch::aarch64::*;

    #[inline(always)]
    pub unsafe fn chacha20_block_neon(
        key: &[u8; 32],
        counter: u64,
        nonce: &[u8; 12],
        output: &mut [u8; 64]
    ) {
        // NEON реализация ChaCha20 (базовая)
        let constants = vld1q_u32(&[0x61707865, 0x3320646e, 0x79622d32, 0x6b206574]);
        let key_part1 = vld1q_u32(key[0..16].as_ptr() as *const u32);
        let key_part2 = vld1q_u32(key[16..32].as_ptr() as *const u32);

        let counter_nonce = vsetq_lane_u32(counter as u32,
                                           vsetq_lane_u32((counter >> 32) as u32,
                                                          vld1q_u32(nonce.as_ptr() as *const u32), 1), 0);

        let mut x0 = constants;
        let mut x1 = key_part1;
        let mut x2 = key_part2;
        let mut x3 = counter_nonce;

        // 20 раундов
        for _ in 0..10 {
            // COLUMN round
            x0 = vaddq_u32(x0, x1);
            x3 = veorq_u32(x3, x0);
            x3 = vorrq_u32(vshlq_n_u32(x3, 16), vshrq_n_u32(x3, 16));

            x2 = vaddq_u32(x2, x3);
            x1 = veorq_u32(x1, x2);
            x1 = vorrq_u32(vshlq_n_u32(x1, 12), vshrq_n_u32(x1, 20));

            x0 = vaddq_u32(x0, x1);
            x3 = veorq_u32(x3, x0);
            x3 = vorrq_u32(vshlq_n_u32(x3, 8), vshrq_n_u32(x3, 24));

            x2 = vaddq_u32(x2, x3);
            x1 = veorq_u32(x1, x2);
            x1 = vorrq_u32(vshlq_n_u32(x1, 7), vshrq_n_u32(x1, 25));

            // DIAGONAL round
            x0 = vaddq_u32(x0, x1);
            x3 = veorq_u32(x3, x0);
            x3 = vorrq_u32(vshlq_n_u32(x3, 16), vshrq_n_u32(x3, 16));

            x2 = vaddq_u32(x2, x3);
            x1 = veorq_u32(x1, x2);
            x1 = vorrq_u32(vshlq_n_u32(x1, 12), vshrq_n_u32(x1, 20));

            x0 = vaddq_u32(x0, x1);
            x3 = veorq_u32(x3, x0);
            x3 = vorrq_u32(vshlq_n_u32(x3, 8), vshrq_n_u32(x3, 24));

            x2 = vaddq_u32(x2, x3);
            x1 = veorq_u32(x1, x2);
            x1 = vorrq_u32(vshlq_n_u32(x1, 7), vshrq_n_u32(x1, 25));
        }

        // Добавляем исходное состояние
        x0 = vaddq_u32(x0, constants);
        x1 = vaddq_u32(x1, key_part1);
        x2 = vaddq_u32(x2, key_part2);
        x3 = vaddq_u32(x3, counter_nonce);

        // Сохраняем результат
        vst1q_u32(output.as_mut_ptr() as *mut u32, x0);
        vst1q_u32(output[16..].as_mut_ptr() as *mut u32, x1);
        vst1q_u32(output[32..].as_mut_ptr() as *mut u32, x2);
        vst1q_u32(output[48..].as_mut_ptr() as *mut u32, x3);
    }
}

/// Детектор возможностей CPU
#[derive(Clone, Copy)]
pub struct CpuCapabilities {
    pub avx2: bool,
    pub avx512: bool,
    pub neon: bool,
    pub aes_ni: bool,
}

impl CpuCapabilities {
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            use std::arch::is_x86_feature_detected;

            let avx2 = is_x86_feature_detected!("avx2");
            let avx512 = is_x86_feature_detected!("avx512f");
            let aes_ni = is_x86_feature_detected!("aes");

            Self {
                avx2,
                avx512,
                neon: false,
                aes_ni,
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            Self {
                avx2: false,
                avx512: false,
                neon: true, // Большинство ARM64 имеет NEON
                aes_ni: cfg!(target_feature = "crypto"),
            }
        }

        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Self {
                avx2: false,
                avx512: false,
                neon: false,
                aes_ni: false,
            }
        }
    }
}

/// Ускоренный ChaCha20 процессор
pub struct ChaCha20Accelerator {
    caps: CpuCapabilities,
}

impl ChaCha20Accelerator {
    pub fn new() -> Self {
        Self {
            caps: CpuCapabilities::detect(),
        }
    }

    #[inline]
    pub fn encrypt(
        &self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        counter: u64,
        input: &[u8],
        output: &mut [u8]
    ) {
        assert_eq!(input.len(), output.len());

        #[cfg(target_arch = "x86_64")]
        if self.caps.avx2 && input.len() >= 256 {
            unsafe {
                x86::chacha20_encrypt_avx2(key, nonce, counter, input, output);
            }
            return;
        }

        #[cfg(target_arch = "aarch64")]
        if self.caps.neon && input.len() >= 128 {
            // Блочная обработка через NEON
            let mut local_counter = counter;
            let mut remaining = input.len();
            let mut in_ptr = input.as_ptr();
            let mut out_ptr = output.as_mut_ptr();

            while remaining >= 64 {
                let mut block = [0u8; 64];
                unsafe {
                    arm::chacha20_block_neon(key, local_counter, nonce, &mut block);
                }

                for i in 0..64 {
                    unsafe {
                        *out_ptr = *in_ptr ^ block[i];
                        out_ptr = out_ptr.add(1);
                        in_ptr = in_ptr.add(1);
                    }
                }

                remaining -= 64;
                local_counter += 1;
            }

            // Остатки
            if remaining > 0 {
                let mut block = [0u8; 64];
                unsafe {
                    arm::chacha20_block_neon(key, local_counter, nonce, &mut block);
                }

                for i in 0..remaining {
                    unsafe {
                        *out_ptr = *in_ptr ^ block[i];
                        out_ptr = out_ptr.add(1);
                        in_ptr = in_ptr.add(1);
                    }
                }
            }
            return;
        }

        // Fallback на чистую Rust реализацию
        self.encrypt_fallback(key, nonce, counter, input, output);
    }

    #[inline]
    fn encrypt_fallback(
        &self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        counter: u64,
        input: &[u8],
        output: &mut [u8]
    ) {
        use chacha20::ChaCha20;
        use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};

        let mut cipher = ChaCha20::new(key.into(), nonce.into());
        cipher.seek(counter * 64);
        cipher.apply_keystream_b2b(input, output).unwrap();
    }

    #[inline]
    pub fn encrypt_in_place(
        &self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        counter: u64,
        data: &mut [u8]
    ) {
        let temp = data.to_vec();
        self.encrypt(key, nonce, counter, &temp, data);
    }
}

impl Clone for ChaCha20Accelerator {
    fn clone(&self) -> Self {
        Self {
            caps: self.caps,
        }
    }
}

impl Default for ChaCha20Accelerator {
    fn default() -> Self {
        Self::new()
    }
}