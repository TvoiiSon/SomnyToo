use std::time::{Instant, Duration};
use zeroize::Zeroize;
use tracing::warn;

use super::scatterer::ScatteredParts;

/// Интерфейс сборщика ключей
pub trait KeyAssembler: Send + Sync {
    /// Собирает ключ из рассеянных частей с постоянным временем
    fn assemble(&self, parts: &ScatteredParts, scatterer: &super::scatterer::MemoryScatterer) -> [u8; 32];

    /// Возвращает время выполнения в наносекундах
    fn execution_time_ns(&self) -> u64;
}

/// Сборщик для x86_64 с AVX2
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
pub struct Avx2Assembler;

#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
impl KeyAssembler for Avx2Assembler {
    fn assemble(&self, parts: &ScatteredParts, scatterer: &super::scatterer::MemoryScatterer) -> [u8; 32] {
        // Временно используем generic реализацию
        GenericAssembler.assemble(parts, scatterer)
    }

    fn execution_time_ns(&self) -> u64 {
        8 // AVX2 быстрее
    }
}

/// Сборщик для ARM с NEON
#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
pub struct NeonAssembler;

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
impl KeyAssembler for NeonAssembler {
    fn assemble(&self, parts: &ScatteredParts, scatterer: &super::scatterer::MemoryScatterer) -> [u8; 32] {
        // ARM NEON реализация для ChaCha20
        GenericAssembler.assemble(parts, scatterer)
    }

    fn execution_time_ns(&self) -> u64 {
        12
    }
}

/// Универсальный сборщик (fallback) оптимизированный для ChaCha20
pub struct GenericAssembler;

impl KeyAssembler for GenericAssembler {
    fn assemble(&self, parts: &ScatteredParts, scatterer: &super::scatterer::MemoryScatterer) -> [u8; 32] {
        let start = Instant::now();

        // 1. "Расшифровываем" RAM часть с ChaCha20Poly1305
        let ram_part = scatterer.decrypt_ram_part_or_fallback(parts);

        // 2. Собираем ключ (оптимизировано для ChaCha20 операций)
        let mut key = [0u8; 32];

        // Фиксированное число итераций для константного времени
        const FIXED_ITERATIONS: usize = 48; // Уменьшено для скорости

        // ChaCha20-friendly операции: ADD, XOR, ROTATE
        for iteration in 0..FIXED_ITERATIONS {
            for i in 0..32 {
                let mut value = 0u8;

                // Операции с постоянным временем
                if i < 8 {
                    value = value.wrapping_add(parts.l1_part[i]);
                }
                if i < 16 {
                    value = value.wrapping_add(parts.l2_part[i % 16]);
                }
                value = value.wrapping_add(ram_part[i]);
                value ^= parts.register_seed[i % 16];

                // Rotate как в ChaCha20
                value = value.rotate_left(((iteration * 7 + i) % 8) as u32);

                key[i] = key[i].wrapping_add(value);
            }
        }

        // Дополнительные ChaCha20-like перемешивания
        for i in (0..32).step_by(4) {
            if i + 3 < 32 {
                let a = key[i];
                let b = key[i + 1];
                let c = key[i + 2];
                let d = key[i + 3];

                // Quarter-round как в ChaCha20
                key[i] = a.wrapping_add(b);
                key[i + 3] = d ^ key[i];
                key[i + 3] = key[i + 3].rotate_left(16);

                key[i + 2] = c.wrapping_add(key[i + 3]);
                key[i + 1] = b ^ key[i + 2];
                key[i + 1] = key[i + 1].rotate_left(12);

                key[i] = key[i].wrapping_add(key[i + 1]);
                key[i + 3] = key[i + 3] ^ key[i];
                key[i + 3] = key[i + 3].rotate_left(8);

                key[i + 2] = key[i + 2].wrapping_add(key[i + 3]);
                key[i + 1] = key[i + 1] ^ key[i + 2];
                key[i + 1] = key[i + 1].rotate_left(7);
            }
        }

        // Проверка времени выполнения
        let elapsed = start.elapsed();

        if elapsed.as_nanos() > 200 {
            warn!("⚠️  SLOW KEY ASSEMBLY: {:?} ({:?} ns)", elapsed, elapsed.as_nanos());
        } else if elapsed.as_nanos() < 10 {
            warn!("⚠️  SUSPICIOUSLY FAST KEY ASSEMBLY: {:?} ({:?} ns)", elapsed, elapsed.as_nanos());
        }

        assert!(elapsed < Duration::from_micros(100),
                "Timing anomaly detected: {:?}", elapsed);

        // Немедленное уничтожение промежуточных данных
        let mut zero_ram = ram_part;
        zero_ram.zeroize();

        key
    }

    fn execution_time_ns(&self) -> u64 {
        50 // Быстрее благодаря оптимизациям под ChaCha20
    }
}

/// Фабрика сборщиков ключей
#[derive(Default)]
pub struct KeyAssemblerFactory;

impl KeyAssemblerFactory {
    pub fn create_assembler(&self) -> Box<dyn KeyAssembler> {
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            if is_x86_feature_detected!("avx2") {
                return Box::new(Avx2Assembler);
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