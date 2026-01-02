use std::sync::{atomic::{AtomicU64, Ordering}};
use std::time::{Instant, Duration};
use zeroize::Zeroize;
use hkdf::Hkdf;
use sha2::Sha256;
use rand_core::{OsRng, RngCore};
use tracing::{info, debug};

use super::scatterer::{ScatteredParts, MemoryScatterer};

/// Фантомный мастер-ключ сессии
pub struct PhantomMasterKey {
    // Рассеянные части мастер-ключа
    pub(crate) scattered_parts: ScatteredParts,

    // ДЕТЕРМИНИРОВАННЫЙ seed для генерации операционных ключей
    // Должен быть одинаковым на клиенте и сервере!
    pub(crate) operation_seed: [u8; 32],

    // Метаданные сессии
    pub(crate) session_id: [u8; 16],
    pub(crate) created_at: Instant,

    // Счетчики операций
    pub(crate) sequence_number: AtomicU64,
    pub(crate) operation_count: AtomicU64,
}

impl PhantomMasterKey {
    pub fn new(scattered_parts: ScatteredParts, session_id: [u8; 16], operation_seed: [u8; 32]) -> Self {
        info!("Creating new phantom master key for session: {}", hex::encode(session_id));
        debug!("Operation seed (first 8 bytes): {}", hex::encode(&operation_seed[..8]));

        Self {
            scattered_parts,
            operation_seed,
            session_id,
            created_at: Instant::now(),
            sequence_number: AtomicU64::new(0),
            operation_count: AtomicU64::new(0),
        }
    }
}

impl Zeroize for PhantomMasterKey {
    fn zeroize(&mut self) {
        info!("Zeroizing phantom master key for session: {}", hex::encode(self.session_id));
        self.scattered_parts.zeroize();
        self.operation_seed.zeroize();
        self.session_id.zeroize();
        self.sequence_number.store(0, Ordering::SeqCst);
        self.operation_count.store(0, Ordering::SeqCst);
    }
}

impl Drop for PhantomMasterKey {
    fn drop(&mut self) {
        self.zeroize();
    }
}

/// Операционный фантом-ключ (живет несколько миллисекунд)
pub struct PhantomOperationKey {
    pub(crate) key_bytes: [u8; 32],
    pub(crate) created_at: Instant,
    pub(crate) sequence: u64,
}

impl Zeroize for PhantomOperationKey {
    fn zeroize(&mut self) {
        self.key_bytes.zeroize();
    }
}

impl Drop for PhantomOperationKey {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl PhantomOperationKey {
    /// Создает новый операционный ключ
    pub fn new(key_bytes: [u8; 32], sequence: u64) -> Self {
        debug!("Creating new phantom operation key with sequence: {}", sequence);

        Self {
            key_bytes,
            created_at: Instant::now(),
            sequence,
        }
    }

    /// Проверяет, не истекло ли время жизни ключа
    pub fn is_expired(&self) -> bool {
        let expired = self.created_at.elapsed() > Duration::from_millis(10);
        if expired {
            debug!("Phantom operation key expired (sequence: {})", self.sequence);
        }
        expired
    }

    /// Получает байты ключа
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.key_bytes
    }
}

/// Фантомная сессия
pub struct PhantomSession {
    pub(crate) master_key: PhantomMasterKey,
    pub(crate) handshake_completed: bool,
}

impl PhantomSession {
    /// Создает новую тестовую сессию (только для тестов)
    pub fn new() -> Self {
        let mut session_id = [0u8; 16];
        OsRng.fill_bytes(&mut session_id);

        let mut operation_seed = [0u8; 32];
        OsRng.fill_bytes(&mut operation_seed);

        let scatterer = MemoryScatterer::new();
        let master_key_bytes = [0u8; 32];
        let scattered_parts = scatterer.scatter(&master_key_bytes);

        info!("Creating new phantom test session: {}", hex::encode(session_id));

        Self {
            master_key: PhantomMasterKey::new(scattered_parts, session_id, operation_seed),
            handshake_completed: false,
        }
    }

    /// Создает новую сессию из общего секрета X25519
    pub fn from_dh_shared(
        shared_secret: &[u8; 32],
        client_nonce: &[u8; 16],
        server_nonce: &[u8; 16],
        client_pub_key: &[u8; 32],
        server_pub_key: &[u8; 32],
    ) -> Self {
        info!("Creating phantom session from DH shared secret");

        // 1. Создаем соль для HKDF
        let mut salt = Vec::with_capacity(96);
        salt.extend_from_slice(client_pub_key);
        salt.extend_from_slice(server_pub_key);
        salt.extend_from_slice(client_nonce);
        salt.extend_from_slice(server_nonce);

        // 2. Генерируем мастер-ключ с помощью HKDF
        let hk = Hkdf::<Sha256>::new(Some(&salt), shared_secret);

        let mut master_key_bytes = [0u8; 32];
        hk.expand(b"phantom-master-key", &mut master_key_bytes)
            .expect("HKDF expansion failed");

        // 3. Генерируем session_id
        let mut session_id = [0u8; 16];
        hk.expand(b"session-id", &mut session_id)
            .expect("HKDF session id");

        // 4. Генерируем ДЕТЕРМИНИРОВАННЫЙ operation_seed
        // Это ключевое изменение - seed должен быть одинаковым на клиенте и сервере!
        let mut operation_seed = [0u8; 32];
        let mut seed_input = Vec::new();
        seed_input.extend_from_slice(shared_secret);
        seed_input.extend_from_slice(&session_id);

        // Используем HKDF для создания детерминированного seed
        let seed_hk = Hkdf::<Sha256>::new(None, &seed_input);
        seed_hk.expand(b"phantom-operation-seed", &mut operation_seed)
            .expect("HKDF expansion failed");

        debug!("Generated operation seed (first 8 bytes): {}", hex::encode(&operation_seed[..8]));

        // 5. Рассеиваем мастер-ключ
        let scatterer = MemoryScatterer::new();
        let scattered_parts = scatterer.scatter(&master_key_bytes);

        // 6. Немедленно уничтожаем сырые байты мастер-ключа
        master_key_bytes.zeroize();

        let master_key = PhantomMasterKey::new(scattered_parts, session_id, operation_seed);

        info!("Phantom session created: {}", hex::encode(master_key.session_id));

        Self {
            master_key,
            handshake_completed: true,
        }
    }

    /// Генерирует операционный ключ для конкретной операции
    pub fn generate_operation_key(&self, operation_type: &str) -> PhantomOperationKey {
        // Увеличиваем счетчик операций
        let sequence = self.master_key.sequence_number.fetch_add(1, Ordering::SeqCst);

        // Увеличиваем счетчик всех операций
        self.master_key.operation_count.fetch_add(1, Ordering::SeqCst);

        self.generate_operation_key_for_sequence(sequence, operation_type)
    }

    /// Генерирует операционный ключ для конкретной последовательности
    pub fn generate_operation_key_for_sequence(&self, sequence: u64, operation_type: &str) -> PhantomOperationKey {
        // Увеличиваем счетчик всех операций
        self.master_key.operation_count.fetch_add(1, Ordering::SeqCst);

        debug!(
            "Generating phantom operation key for sequence: session={}, sequence={}, type={}",
            hex::encode(self.master_key.session_id),
            sequence,
            operation_type
        );

        // Создаем seed для этой операции
        let mut seed = Vec::new();
        seed.extend_from_slice(&self.master_key.session_id);
        seed.extend_from_slice(&sequence.to_be_bytes());
        seed.extend_from_slice(operation_type.as_bytes());

        debug!("HKDF seed ({} bytes): {}", seed.len(), hex::encode(&seed));
        debug!("Operation seed (first 8 bytes): {}",
               hex::encode(&self.master_key.operation_seed[..8]));

        // Используем HKDF для генерации ключа операции с ДЕТЕРМИНИРОВАННЫМ seed
        let hk = Hkdf::<Sha256>::new(Some(&seed), &self.master_key.operation_seed);
        let mut operation_key_bytes = [0u8; 32];
        hk.expand(b"phantom-operation-key", &mut operation_key_bytes)
            .expect("HKDF expansion failed");

        // Создаем операционный ключ
        PhantomOperationKey::new(operation_key_bytes, sequence)
    }

    /// Получает текущую последовательность
    pub fn current_sequence(&self) -> u64 {
        self.master_key.sequence_number.load(Ordering::SeqCst)
    }

    /// Проверяет валидность сессии
    pub fn is_valid(&self) -> bool {
        let age = self.master_key.created_at.elapsed();
        let is_valid = age < Duration::from_secs(90) &&
            self.handshake_completed &&
            self.master_key.operation_count.load(Ordering::SeqCst) < 1_000_000;

        if !is_valid {
            info!("Phantom session invalid: {}", hex::encode(self.master_key.session_id));
        }

        is_valid
    }

    /// Получает ID сессии
    pub fn session_id(&self) -> &[u8; 16] {
        &self.master_key.session_id
    }

    /// Получает статистику сессии
    pub fn stats(&self) -> SessionStats {
        SessionStats {
            session_id: self.master_key.session_id,
            created_at: self.master_key.created_at,
            operation_count: self.master_key.operation_count.load(Ordering::SeqCst),
            is_valid: self.is_valid(),
        }
    }

    /// DEBUG: Получает operation seed для отладки
    pub fn get_operation_seed(&self) -> [u8; 32] {
        self.master_key.operation_seed
    }
}

/// Статистика сессии
#[derive(Debug)]
pub struct SessionStats {
    pub session_id: [u8; 16],
    pub created_at: Instant,
    pub operation_count: u64,
    pub is_valid: bool,
}

impl Default for PhantomSession {
    fn default() -> Self {
        Self::new()
    }
}