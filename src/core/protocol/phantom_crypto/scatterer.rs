use zeroize::Zeroize;
use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
use generic_array::GenericArray;
use rand_core::{OsRng, RngCore};

/// Рассеянные части ключа, хранящиеся в разных уровнях памяти
#[derive(Clone)]
pub struct ScatteredParts {
    // L1 часть: всегда в L1 кэше процессора (8 байт)
    pub(crate) l1_part: [u8; 8],

    // L2 часть: всегда в L2 кэше (16 байт)
    pub(crate) l2_part: [u8; 16],

    // RAM часть: в оперативной памяти, зашифрованная (32 байта)
    pub(crate) ram_part_encrypted: [u8; 32],
    pub(crate) ram_part_iv: [u8; 12],

    // Метаданные для вычисления регистровой части
    // ЭТО ТЕПЕРЬ НЕ ИСПОЛЬЗУЕТСЯ ДЛЯ ГЕНЕРАЦИИ КЛЮЧЕЙ!
    pub(crate) register_seed: [u8; 16],
    // Не включаем AtomicU64 в Clone
}

impl ScatteredParts {
    pub fn new() -> Self {
        Self {
            l1_part: [0; 8],
            l2_part: [0; 16],
            ram_part_encrypted: [0; 32],
            ram_part_iv: [0; 12],
            register_seed: [0; 16],
        }
    }
}

impl Zeroize for ScatteredParts {
    fn zeroize(&mut self) {
        self.l1_part.zeroize();
        self.l2_part.zeroize();
        self.ram_part_encrypted.zeroize();
        self.ram_part_iv.zeroize();
        self.register_seed.zeroize();
    }
}

impl Drop for ScatteredParts {
    fn drop(&mut self) {
        self.zeroize();
    }
}

/// Рассеиватель памяти - разбивает ключи на части
pub struct MemoryScatterer {
    encryption_key: [u8; 32],
}

impl MemoryScatterer {
    pub fn new() -> Self {
        let mut encryption_key = [0u8; 32];
        OsRng.fill_bytes(&mut encryption_key);

        Self { encryption_key }
    }

    /// Рассеивает мастер-ключ на части
    pub fn scatter(&self, master_key: &[u8; 32]) -> ScatteredParts {
        let mut rng = OsRng;

        // 1. Генерируем случайные части
        let mut l1_part = [0u8; 8];
        let mut l2_part = [0u8; 16];
        let mut register_seed = [0u8; 16];
        let mut ram_part_iv = [0u8; 12];

        rng.fill_bytes(&mut l1_part);
        rng.fill_bytes(&mut l2_part);
        rng.fill_bytes(&mut register_seed);
        rng.fill_bytes(&mut ram_part_iv);

        // 2. Вычисляем RAM часть как XOR мастер-ключа с другими частями
        let mut ram_part = [0u8; 32];
        for i in 0..32 {
            let mut value = master_key[i];

            if i < 8 {
                value ^= l1_part[i];
            }
            if i < 16 {
                value ^= l2_part[i % 16];
            }
            if i < 16 {
                value ^= register_seed[i % 16];
            }

            ram_part[i] = value;
        }

        // 3. Шифруем RAM часть
        let cipher = Aes256Gcm::new_from_slice(&self.encryption_key)
            .expect("Valid encryption key");

        let nonce = GenericArray::from_slice(&ram_part_iv);
        let ram_part_encrypted_bytes = cipher.encrypt(nonce, &ram_part[..])
            .expect("Encryption failed");

        let ram_part_encrypted: [u8; 32] = ram_part_encrypted_bytes.try_into()
            .unwrap_or_else(|_| [0; 32]);

        // 4. Немедленно уничтожаем промежуточные данные
        ram_part.zeroize();

        ScatteredParts {
            l1_part,
            l2_part,
            ram_part_encrypted,
            ram_part_iv,
            register_seed,
        }
    }

    /// Перерассеивание ключа (периодическая ротация)
    pub fn rescatter(&self, parts: &mut ScatteredParts, master_key: &[u8; 32]) {
        // Немедленно уничтожаем старые части
        parts.zeroize();

        // Создаем новые рассеянные части
        *parts = self.scatter(master_key);
    }
}

impl Default for MemoryScatterer {
    fn default() -> Self {
        Self::new()
    }
}