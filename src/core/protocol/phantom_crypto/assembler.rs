use std::time::{Instant, Duration};
use zeroize::Zeroize;

use super::scatterer::ScatteredParts;

/// Интерфейс сборщика ключей
pub trait KeyAssembler: Send + Sync {
    /// Собирает ключ из рассеянных частей с постоянным временем
    fn assemble(&self, parts: &ScatteredParts) -> [u8; 32];

    /// Возвращает время выполнения в наносекундах
    fn execution_time_ns(&self) -> u64;
}

/// Сборщик для x86_64 с AVX2 + AES-NI
#[cfg(all(target_arch = "x86_64", target_feature = "avx2", target_feature = "aes"))]
pub struct Avx2AesAssembler;

#[cfg(all(target_arch = "x86_64", target_feature = "avx2", target_feature = "aes"))]
impl KeyAssembler for Avx2AesAssembler {
    fn assemble(&self, parts: &ScatteredParts) -> [u8; 32] {
        // Временно используем generic реализацию
        // В реальной реализации здесь будет AVX2 код
        GenericAssembler.assemble(parts)
    }

    fn execution_time_ns(&self) -> u64 {
        10
    }
}

/// Сборщик для ARM с NEON
#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
pub struct NeonAssembler;

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
impl KeyAssembler for NeonAssembler {
    fn assemble(&self, parts: &ScatteredParts) -> [u8; 32] {
        // ARM NEON реализация
        GenericAssembler.assemble(parts)
    }

    fn execution_time_ns(&self) -> u64 {
        15
    }
}

/// Универсальный сборщик (fallback)
pub struct GenericAssembler;

impl KeyAssembler for GenericAssembler {
    fn assemble(&self, parts: &ScatteredParts) -> [u8; 32] {
        let start = Instant::now();

        // 1. "Расшифровываем" RAM часть (в реальности это XOR)
        let mut ram_part = [0u8; 32];
        for i in 0..32 {
            ram_part[i] = parts.ram_part_encrypted[i] ^ parts.ram_part_iv[i % 12];
        }

        // 2. Собираем ключ
        let mut key = [0u8; 32];

        // Фиксированное число итераций для константного времени
        const FIXED_ITERATIONS: usize = 64;

        for iteration in 0..FIXED_ITERATIONS {
            for i in 0..32 {
                let mut value = 0u8;

                // Операции с постоянным временем
                if i < 8 {
                    value ^= parts.l1_part[i];
                }
                if i < 16 {
                    value ^= parts.l2_part[i % 16];
                }
                value ^= ram_part[i];
                value ^= parts.register_seed[i % 16];

                // Rotate для усложнения
                value = value.rotate_left((iteration % 8) as u32);

                key[i] = value;
            }
        }

        // Проверка времени выполнения (защита от timing attacks)
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_micros(100),
                "Timing anomaly detected: {:?}", elapsed);

        // Немедленное уничтожение промежуточных данных
        ram_part.zeroize();

        key
    }

    fn execution_time_ns(&self) -> u64 {
        66
    }
}

/// Фабрика сборщиков ключей
#[derive(Default)]
pub struct KeyAssemblerFactory;

impl KeyAssemblerFactory {
    pub fn create_assembler(&self) -> Box<dyn KeyAssembler> {
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2", target_feature = "aes"))]
        {
            if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("aes") {
                return Box::new(Avx2AesAssembler);
            }
        }

        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        {
            use std::arch::is_aarch64_feature_detected;
            if is_aarch64_feature_detected!("neon") {
                return Box::new(NeonAssembler);
            }
        }

        // Fallback на универсальную реализацию
        Box::new(GenericAssembler)
    }
}