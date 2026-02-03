use std::sync::{atomic::{AtomicU64, Ordering}};
use std::time::{Instant, Duration};
use zeroize::Zeroize;
use blake3::Hasher;
use rand_core::{OsRng, RngCore};
use tracing::{info, debug};

use crate::core::protocol::phantom_crypto::memory::scatterer::{ScatteredParts, MemoryScatterer};

/// Фантомный мастер-ключ сессии
pub struct PhantomMasterKey {
    // Рассеянные части мастер-ключа
    pub(crate) scattered_parts: ScatteredParts,

    // ДЕТЕРМИНИРОВАННЫЙ seed для генерации операционных ключей
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
        Self {
            scattered_parts,
            operation_seed,
            session_id,
            created_at: Instant::now(),
            sequence_number: AtomicU64::new(0),
            operation_count: AtomicU64::new(0),
        }
    }

    /// Генерация мастер-ключа с Blake3 (вместо HKDF)
    pub fn from_dh_shared_blake3(
        shared_secret: &[u8; 32],
        client_nonce: &[u8; 16],
        server_nonce: &[u8; 16],
        client_pub_key: &[u8; 32],
        server_pub_key: &[u8; 32],
    ) -> (ScatteredParts, [u8; 16], [u8; 32]) {
        let mut stages_time = Vec::new();

        // Blake3 для деривации ключей
        let hashing_start = Instant::now();
        let mut hasher = Hasher::new();
        hasher.update(shared_secret);
        hasher.update(client_nonce);
        hasher.update(server_nonce);
        hasher.update(client_pub_key);
        hasher.update(server_pub_key);

        // Выводим все необходимые данные за один проход
        let mut output = [0u8; 32 + 16 + 32]; // master_key + session_id + operation_seed
        hasher.finalize_xof().fill(&mut output);

        let master_key: [u8; 32] = output[0..32].try_into().unwrap();
        let session_id: [u8; 16] = output[32..48].try_into().unwrap();
        let operation_seed: [u8; 32] = output[48..80].try_into().unwrap();
        let hashing_time = hashing_start.elapsed();
        stages_time.push(("key_hashing", hashing_time));

        // Рассеиваем мастер-ключ
        let scattering_start = Instant::now();
        let scatterer = MemoryScatterer::new();
        let scattered_parts = scatterer.scatter(&master_key);
        let scattering_time = scattering_start.elapsed();
        stages_time.push(("memory_scattering", scattering_time));

        // Немедленно уничтожаем сырые байты мастер-ключа
        let mut zero_master = master_key;
        zero_master.zeroize();

        (scattered_parts, session_id, operation_seed)
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
        let (scattered_parts, session_id, operation_seed) =
            PhantomMasterKey::from_dh_shared_blake3(
                shared_secret,
                client_nonce,
                server_nonce,
                client_pub_key,
                server_pub_key
            );

        let master_key = PhantomMasterKey::new(scattered_parts, session_id, operation_seed);

        Self {
            master_key,
            handshake_completed: true,
        }
    }

    /// Генерирует операционный ключ для конкретной операции
    pub fn generate_operation_key(&self, operation_type: &str) -> PhantomOperationKey {
        let mut stages_time = Vec::new();

        let sequence = self.master_key.sequence_number.fetch_add(1, Ordering::SeqCst);
        self.master_key.operation_count.fetch_add(1, Ordering::SeqCst);

        let actual_operation_type = if operation_type == "encrypt" {
            "encrypt"
        } else {
            operation_type
        };

        // Blake3 для деривации операционного ключа
        let hashing_start = Instant::now();
        let mut hasher = Hasher::new();
        hasher.update(&self.master_key.session_id);
        hasher.update(&sequence.to_be_bytes());
        hasher.update(actual_operation_type.as_bytes());
        hasher.update(&self.master_key.operation_seed);

        let mut operation_key_bytes = [0u8; 32];
        hasher.finalize_xof().fill(&mut operation_key_bytes);
        let hashing_time = hashing_start.elapsed();
        stages_time.push(("key_hashing", hashing_time));

        // Создаем операционный ключ
        let creation_start = Instant::now();
        let key = PhantomOperationKey::new(operation_key_bytes, sequence);
        let creation_time = creation_start.elapsed();
        stages_time.push(("key_object_creation", creation_time));

        key
    }

    /// Генерирует операционный ключ для конкретной последовательности
    pub fn generate_operation_key_for_sequence(&self, sequence: u64, operation_type: &str) -> PhantomOperationKey {
        let start = Instant::now();

        let actual_operation_type = if operation_type == "encrypt" {
            "encrypt"
        } else {
            operation_type
        };

        // Blake3 для деривации операционного ключа
        let mut hasher = Hasher::new();
        hasher.update(&self.master_key.session_id);
        hasher.update(&sequence.to_be_bytes());
        hasher.update(actual_operation_type.as_bytes());
        hasher.update(&self.master_key.operation_seed);

        let mut operation_key_bytes = [0u8; 32];
        hasher.finalize_xof().fill(&mut operation_key_bytes);

        let elapsed = start.elapsed();
        debug!("Key generation for sequence {}: {:?} ({:.2} µs)",
               sequence, elapsed, elapsed.as_nanos() as f64 / 1000.0);

        PhantomOperationKey::new(operation_key_bytes, sequence)
    }

    /// Получает текущую последовательность
    pub fn current_sequence(&self) -> u64 {
        self.master_key.sequence_number.load(Ordering::SeqCst)
    }

    /// Получает следующий номер последовательности
    pub fn next_sequence(&self) -> u64 {
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

    /// Получает мастер-ключ
    pub fn master_key(&self) -> &PhantomMasterKey {
        &self.master_key
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