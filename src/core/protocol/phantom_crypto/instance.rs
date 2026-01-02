use super::{
    scatterer::MemoryScatterer,
    keys::PhantomSession,
    runtime::PhantomRuntime,
};

/// Главный интерфейс фантомной криптосистемы
pub struct PhantomCrypto {
    runtime: PhantomRuntime,
    scatterer: MemoryScatterer,
}

impl PhantomCrypto {
    pub fn new() -> Self {
        let runtime = PhantomRuntime::new();
        let scatterer = MemoryScatterer::new();

        Self {
            runtime,
            scatterer,
        }
    }

    /// Создание новой сессии
    pub fn create_session(&self) -> PhantomSession {
        PhantomSession::new()
    }

    /// Рассеивание мастер-ключа
    pub fn scatter_master_key(&self, master_key: &[u8; 32]) -> super::scatterer::ScatteredParts {
        self.scatterer.scatter(master_key)
    }

    /// Получает runtime
    pub fn runtime(&self) -> &PhantomRuntime {
        &self.runtime
    }
}

/// Конфигурация фантомной системы
pub struct PhantomConfig {
    pub session_timeout_ms: u64,
    pub max_sessions: usize,
    pub enable_hardware_acceleration: bool,
    pub constant_time_enforced: bool,
}

impl Default for PhantomConfig {
    fn default() -> Self {
        Self {
            session_timeout_ms: 90_000, // 90 секунд
            max_sessions: 100_000,
            enable_hardware_acceleration: true,
            constant_time_enforced: true,
        }
    }
}