use std::env;
use hkdf::Hkdf;
use sha2::Sha256;
use tracing::info;
use zeroize::Zeroize;
use aes_gcm::{Aes256Gcm, KeyInit};

#[derive(Clone)]
pub struct SessionKeys {
    pub aead_key_bytes: [u8; 32],
    pub sign_key: [u8; 32],
    pub session_id: [u8; 16],
    pub aead_cipher: Aes256Gcm,
}

impl Zeroize for SessionKeys {
    fn zeroize(&mut self) {
        self.sign_key.zeroize();
        self.session_id.zeroize();
    }
}

impl Drop for SessionKeys {
    fn drop(&mut self) {
        self.zeroize();
    }
}

// Memory-safe buffers
pub struct SecureBuffer {
    inner: Vec<u8>,
}

impl SecureBuffer {
    pub fn new(data: Vec<u8>) -> Self {
        Self { inner: data }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.inner
    }
}

impl SecureBuffer {
    fn zeroize(&mut self) {
        self.inner.zeroize();
    }
}

impl Drop for SecureBuffer {
    fn drop(&mut self) {
        self.zeroize();
    }
}

// CPU feature detection
#[cfg(target_arch = "x86_64")]
fn is_aes_ni_supported() -> bool {
    is_x86_feature_detected!("aes") && is_x86_feature_detected!("sse2")
}

#[cfg(not(target_arch = "x86_64"))]
fn is_aes_ni_supported() -> bool {
    false
}

#[cfg(target_arch = "x86_64")]
fn is_avx2_supported() -> bool {
    is_x86_feature_detected!("avx2")
}

#[cfg(not(target_arch = "x86_64"))]
fn is_avx2_supported() -> bool {
    false
}

impl SessionKeys {
    pub fn from_dh_shared(shared_secret: &[u8; 32], salt: &[u8]) -> Self {
        let psk = env::var("PSK_SECRET")
            .expect("PSK_SECRET environment variable not set");

        let psk_bytes = hex::decode(&psk)
            .expect("PSK_SECRET must be a valid hex string");

        Self::from_dh_shared_with_psk(shared_secret, salt, &psk_bytes)
    }

    /// Версия с PSK для аутентификации
    pub fn from_dh_shared_with_psk(shared_secret: &[u8; 32], salt: &[u8], psk: &[u8]) -> Self {
        // Detect CPU features for optimization
        let use_hw_acceleration = is_aes_ni_supported() && is_avx2_supported();

        let hk = Hkdf::<Sha256>::new(Some(salt), shared_secret);

        let mut aead_key_bytes = [0u8; 32];  // сырые байты ключа
        let mut sign_key = [0u8; 32];
        let mut session_id = [0u8; 16];

        // Генерируем уникальный session_id
        hk.expand(b"session-id", &mut session_id).expect("HKDF session id");

        // Используем PSK в качестве дополнительного контекста
        let mut info_with_psk = Vec::new();
        info_with_psk.extend_from_slice(b"ctx-aead");
        info_with_psk.extend_from_slice(psk);

        hk.expand(&info_with_psk, &mut aead_key_bytes).expect("HKDF aead");
        hk.expand(b"ctx-sign_key", &mut sign_key).expect("HKDF sign key");

        // Создаем шифр (но нам нужно вернуть только байты)
        let aead_cipher = if use_hw_acceleration {
            Aes256Gcm::new_from_slice(&aead_key_bytes).expect("aead key")
        } else {
            // Fallback to software implementation
            Aes256Gcm::new_from_slice(&aead_key_bytes).expect("aead key")
        };

        // Логируем только хэши ключей в debug-режиме
        #[cfg(debug_assertions)]
        {
            use sha2::Digest;
            let aead_hash = Sha256::digest(&aead_key_bytes);
            let sign_hash = Sha256::digest(&sign_key);
            info!(target: "session_keys", "aead_key_hash = {}", hex::encode(aead_hash));
            info!(target: "session_keys", "sign_key_hash = {}", hex::encode(sign_hash));
            info!(target: "session_keys", "session_id = {}", hex::encode(session_id));
            info!(target: "session_keys", "hardware_acceleration = {}", use_hw_acceleration);
        }

        // Возвращаем СЫРЫЕ БАЙТЫ ключей, а не объекты шифров
        Self {
            aead_key_bytes,
            sign_key,
            session_id,
            aead_cipher,
        }
    }

    // ИСПРАВЛЕНИЕ: Делаем метод публичным и возвращаем &[u8]
    pub fn get_sign_key(&self) -> &[u8] {
        &self.sign_key
    }

    // Также добавляем метод для получения aead_key если нужно
    pub fn get_aead_key(&self) -> &[u8; 32] {
        &self.aead_key_bytes
    }
}
