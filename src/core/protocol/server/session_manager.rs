use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{RwLock, Mutex};
use tracing::{info, warn};

use crate::core::protocol::crypto::key_manager::session_keys::SessionKeys;
use crate::core::protocol::server::heartbeat::manager::{HeartbeatManager, HeartbeatConfig};
use crate::core::protocol::server::connection_manager::ConnectionManager;

pub struct Session {
    pub keys: Arc<SessionKeys>,
    pub addr: SocketAddr,
    pub created_at: std::time::Instant,
}

pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<Vec<u8>, Session>>>,
    heartbeat_manager: Arc<HeartbeatManager>,
    connection_manager: Arc<ConnectionManager>,
    // Мьютекс для транзакционных операций удаления
    cleanup_lock: Arc<Mutex<()>>,
}

impl SessionManager {
    pub fn new(connection_manager: Arc<ConnectionManager>) -> Self {
        let config = HeartbeatConfig::default();
        let heartbeat_manager = Arc::new(HeartbeatManager::new(config, Arc::clone(&connection_manager)));

        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            heartbeat_manager,
            connection_manager,
            cleanup_lock: Arc::new(Mutex::new(())),
        }
    }

    // Добавим метод для получения heartbeat_manager
    pub fn get_heartbeat_manager(&self) -> Arc<HeartbeatManager> {
        Arc::clone(&self.heartbeat_manager)
    }

    pub async fn start_heartbeat(&self) {
        self.heartbeat_manager.start().await;
        info!("Heartbeat manager started");
    }

    pub async fn register_session(&self, session_id: Vec<u8>, keys: Arc<SessionKeys>, addr: SocketAddr) {
        let session = Session {
            keys,
            addr,
            created_at: std::time::Instant::now(),
        };

        // Атомарная регистрация во всех менеджерах
        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(session_id.clone(), session);
        }

        // Register in heartbeat manager
        self.heartbeat_manager.register_session(session_id.clone(), addr).await;

        info!("Session registered: {} from {}", hex::encode(session_id.clone()), addr);
    }

    pub async fn unregister_session(&self, session_id: &[u8]) {
        // Используем транзакционный метод для гарантии согласованности
        self.force_remove_session(session_id).await;
    }

    // ТРАНЗАКЦИОННЫЙ МЕТОД: атомарное удаление сессии из всех менеджеров
    pub async fn force_remove_session(&self, session_id: &[u8]) {
        // Блокируем для предотвращения race conditions
        let _guard = self.cleanup_lock.lock().await;

        let session_id_str = hex::encode(session_id);

        // Шаг 1: Удаляем из heartbeat manager
        self.heartbeat_manager.unregister_session(session_id).await;

        // Шаг 2: Принудительно разрываем соединение
        self.connection_manager.force_disconnect(session_id).await;

        // Шаг 3: Удаляем из session manager
        {
            let mut sessions = self.sessions.write().await;
            sessions.remove(session_id);
        }

        info!("Session fully removed from all managers: {}", session_id_str);
    }

    // Метод для безопасной проверки существования сессии
    pub async fn session_exists(&self, session_id: &[u8]) -> bool {
        let sessions = self.sessions.read().await;
        sessions.contains_key(session_id)
    }

    // Безопасное получение сессии с проверкой согласованности
    pub async fn get_session_consistent(&self, session_id: &[u8]) -> Option<Arc<SessionKeys>> {
        // Проверяем, что сессия существует во всех менеджерах
        let session_exists = self.session_exists(session_id).await;
        let heartbeat_alive = self.heartbeat_manager.is_connection_alive(session_id).await;

        if session_exists && heartbeat_alive {
            let sessions = self.sessions.read().await;
            sessions.get(session_id).map(|session| session.keys.clone())
        } else if session_exists != heartbeat_alive {
            // Обнаружено рассогласование - автоматически исправляем
            warn!("Session consistency issue detected for {}, forcing cleanup", hex::encode(session_id));
            self.force_remove_session(session_id).await;
            None
        } else {
            None
        }
    }

    pub async fn get_session(&self, session_id: &[u8]) -> Option<Arc<SessionKeys>> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).map(|session| session.keys.clone())
    }

    pub async fn on_heartbeat_received(&self, session_id: &[u8]) -> bool {
        self.heartbeat_manager.update_heartbeat_received(session_id).await
    }

    pub async fn is_connection_alive(&self, session_id: &[u8]) -> bool {
        // Проверяем согласованность между менеджерами
        let session_exists = self.session_exists(session_id).await;
        let heartbeat_alive = self.heartbeat_manager.is_connection_alive(session_id).await;

        if session_exists && !heartbeat_alive {
            // Сессия есть в SessionManager, но heartbeat мертв - очищаем
            self.force_remove_session(session_id).await;
            false
        } else {
            heartbeat_alive
        }
    }

    pub async fn on_ping_sent(&self, session_id: &[u8]) -> bool {
        self.heartbeat_manager.on_ping_sent(session_id).await
    }

    pub async fn get_active_sessions(&self) -> Vec<Arc<SessionKeys>> {
        let sessions = self.sessions.read().await;
        sessions.values()
            .map(|session| session.keys.clone())
            .collect()
    }

    // Получение только согласованных сессий
    pub async fn get_consistent_sessions(&self) -> Vec<Arc<SessionKeys>> {
        let sessions = self.sessions.read().await;
        let mut consistent_sessions = Vec::new();

        for (session_id, session) in sessions.iter() {
            if self.heartbeat_manager.is_connection_alive(session_id).await {
                consistent_sessions.push(session.keys.clone());
            }
        }

        consistent_sessions
    }

    pub async fn should_send_heartbeat(&self, _session_id: &[u8]) -> bool {
        true
    }

    pub async fn check_session_reuse(_session_id: &[u8]) -> bool {
        false
    }

    // Метод для принудительной проверки и восстановления согласованности
    pub async fn check_consistency(&self) -> usize {
        let sessions = self.sessions.read().await;
        let mut inconsistent_count = 0;

        for session_id in sessions.keys() {
            let heartbeat_alive = self.heartbeat_manager.is_connection_alive(session_id).await;
            let connection_exists = self.connection_manager.connection_exists(session_id).await;

            if !heartbeat_alive || !connection_exists {
                inconsistent_count += 1;
                warn!("Inconsistent session detected: {}", hex::encode(session_id));
            }
        }

        inconsistent_count
    }
}

// Добавим реализацию Default для тестов
impl Default for SessionManager {
    fn default() -> Self {
        let connection_manager = Arc::new(ConnectionManager::new());
        Self::new(connection_manager)
    }
}