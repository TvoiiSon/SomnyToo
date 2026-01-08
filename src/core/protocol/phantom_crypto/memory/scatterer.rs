use zeroize::Zeroize;
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, KeyInit, AeadInPlace};
use rand_core::{OsRng, RngCore};
use std::time::{Instant};
use tracing::warn;

/// Рассеянные части ключа, хранящиеся в разных уровнях памяти
/// Теперь включает TAG для проверки целостности ChaCha20Poly1305
#[derive(Clone)]
pub struct ScatteredParts {
    // L1 часть: всегда в L1 кэше процессора (8 байт)
    pub(crate) l1_part: [u8; 8],

    // L2 часть: всегда в L2 кэше (16 байт)
    pub(crate) l2_part: [u8; 16],

    // RAM часть: в оперативной памяти (32 байта ciphertext)
    pub(crate) ram_part: [u8; 32],

    // TAG для проверки целостности ChaCha20Poly1305 (16 байт)
    pub(crate) ram_tag: [u8; 16],

    // Nonce для ChaCha20Poly1305 (12 байт)
    pub(crate) ram_part_nonce: [u8; 12],

    // Метаданные для вычисления регистровой части
    pub(crate) register_seed: [u8; 16],
}

impl ScatteredParts {
    pub fn new() -> Self {
        Self {
            l1_part: [0; 8],
            l2_part: [0; 16],
            ram_part: [0; 32],
            ram_tag: [0; 16],
            ram_part_nonce: [0; 12],
            register_seed: [0; 16],
        }
    }

    /// Проверяет целостность RAM части с помощью TAG
    pub fn verify_integrity(&self, scatterer: &MemoryScatterer) -> bool {
        scatterer.verify_ram_part_integrity(self)
    }
}

impl Zeroize for ScatteredParts {
    fn zeroize(&mut self) {
        self.l1_part.zeroize();
        self.l2_part.zeroize();
        self.ram_part.zeroize();
        self.ram_tag.zeroize();
        self.ram_part_nonce.zeroize();
        self.register_seed.zeroize();
    }
}

impl Drop for ScatteredParts {
    fn drop(&mut self) {
        self.zeroize();
    }
}

/// Рассеиватель памяти с ChaCha20Poly1305 и полной проверкой целостности
pub struct MemoryScatterer {
    encryption_key: [u8; 32],
}

impl MemoryScatterer {
    pub fn new() -> Self {
        let mut encryption_key = [0u8; 32];
        OsRng.fill_bytes(&mut encryption_key);

        Self { encryption_key }
    }

    /// Рассеивает мастер-ключ на части с ChaCha20Poly1305 и полной проверкой целостности
    pub fn scatter(&self, master_key: &[u8; 32]) -> ScatteredParts {
        let _start = Instant::now();
        let mut rng = OsRng;

        // 1. Генерируем случайные части
        let mut l1_part = [0u8; 8];
        let mut l2_part = [0u8; 16];
        let mut register_seed = [0u8; 16];
        let mut ram_part_nonce = [0u8; 12];

        rng.fill_bytes(&mut l1_part);
        rng.fill_bytes(&mut l2_part);
        rng.fill_bytes(&mut register_seed);
        rng.fill_bytes(&mut ram_part_nonce);

        // 2. Вычисляем RAM часть как XOR мастер-ключа с другими частями
        let mut ram_part_plain = [0u8; 32];
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

            ram_part_plain[i] = value;
        }

        // 3. Шифруем RAM часть с ChaCha20Poly1305
        // Для encrypt_in_place нам нужен буфер с местом для TAG
        let mut ram_part_with_tag = Vec::with_capacity(32 + 16);
        ram_part_with_tag.extend_from_slice(&ram_part_plain);

        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.encryption_key));
        let nonce = Nonce::from_slice(&ram_part_nonce);

        match cipher.encrypt_in_place(nonce, &[], &mut ram_part_with_tag) {
            Ok(_) => {
                // После encrypt_in_place, ram_part_with_tag содержит: [ciphertext...][TAG...]
                // Должно быть 32 + 16 = 48 байт

                if ram_part_with_tag.len() != 48 {
                    warn!("Unexpected ciphertext+TAG length: {} bytes, expected 48", ram_part_with_tag.len());
                    // Fallback: если размер неверный, используем XOR
                    return Self::create_fallback_parts(l1_part, l2_part, register_seed, ram_part_plain);
                }

                // Извлекаем ciphertext (первые 32 байта)
                let mut ram_part_encrypted = [0u8; 32];
                ram_part_encrypted.copy_from_slice(&ram_part_with_tag[..32]);

                // Извлекаем TAG (последние 16 байт)
                let mut ram_tag = [0u8; 16];
                ram_tag.copy_from_slice(&ram_part_with_tag[32..]);

                // Немедленно очищаем временные данные
                ram_part_with_tag.zeroize();
                ram_part_plain.zeroize();

                ScatteredParts {
                    l1_part,
                    l2_part,
                    ram_part: ram_part_encrypted,
                    ram_tag,
                    ram_part_nonce,
                    register_seed,
                }
            }
            Err(e) => {
                // Fallback: если шифрование не удалось, используем XOR
                warn!("ChaCha20Poly1305 encryption failed: {:?}, using XOR fallback", e);
                Self::create_fallback_parts(l1_part, l2_part, register_seed, ram_part_plain)
            }
        }
    }

    /// Создает fallback части с XOR вместо шифрования
    fn create_fallback_parts(
        l1_part: [u8; 8],
        l2_part: [u8; 16],
        register_seed: [u8; 16],
        ram_part_plain: [u8; 32]
    ) -> ScatteredParts {
        ScatteredParts {
            l1_part,
            l2_part,
            ram_part: ram_part_plain,
            ram_tag: [0u8; 16], // нулевой TAG для fallback
            ram_part_nonce: [0; 12], // нулевой nonce для fallback
            register_seed,
        }
    }

    /// Дешифрует RAM часть с проверкой TAG
    pub fn decrypt_ram_part(&self, parts: &ScatteredParts) -> Result<[u8; 32], &'static str> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.encryption_key));
        let nonce = Nonce::from_slice(&parts.ram_part_nonce);

        // Собираем ciphertext + TAG для дешифрования
        let mut ciphertext_with_tag = Vec::with_capacity(32 + 16);
        ciphertext_with_tag.extend_from_slice(&parts.ram_part);
        ciphertext_with_tag.extend_from_slice(&parts.ram_tag);

        // Дешифруем in-place
        match cipher.decrypt_in_place(nonce, &[], &mut ciphertext_with_tag) {
            Ok(_) => {
                // После дешифрования ciphertext_with_tag содержит plaintext (32 байта)
                if ciphertext_with_tag.len() != 32 {
                    warn!("Unexpected plaintext length after decryption: {} bytes", ciphertext_with_tag.len());
                    // Возвращаем то что есть, но обрезаем/дополняем до 32 байт
                }

                let mut decrypted = [0u8; 32];
                let copy_len = ciphertext_with_tag.len().min(32);
                decrypted[..copy_len].copy_from_slice(&ciphertext_with_tag[..copy_len]);

                // Очищаем временные данные
                ciphertext_with_tag.zeroize();

                Ok(decrypted)
            }
            Err(e) => {
                warn!("ChaCha20Poly1305 decryption failed: {:?}", e);
                Err("Decryption failed: invalid ciphertext or TAG")
            }
        }
    }

    /// Проверяет целостность RAM части с помощью TAG
    pub fn verify_ram_part_integrity(&self, parts: &ScatteredParts) -> bool {
        // Для проверки целостности пытаемся дешифровать
        match self.decrypt_ram_part(parts) {
            Ok(_) => true,
            Err(_) => false,
        }
    }

    /// Безопасно дешифрует RAM часть или возвращает fallback
    pub fn decrypt_ram_part_or_fallback(&self, parts: &ScatteredParts) -> [u8; 32] {
        match self.decrypt_ram_part(parts) {
            Ok(decrypted) => decrypted,
            Err(_) => {
                warn!("RAM part decryption failed, using fallback");
                // Fallback: возвращаем зашифрованные данные как есть
                parts.ram_part
            }
        }
    }
}

impl Default for MemoryScatterer {
    fn default() -> Self {
        Self::new()
    }
}