use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::{interval, Instant};
use tracing::{info, warn};

use crate::core::protocol::server::connection_manager::ConnectionManager;

#[derive(Clone, Debug)]
pub struct HeartbeatConfig {
    pub ping_interval: Duration,
    pub timeout: Duration,
    pub max_missed_pings: u32,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(30),
            timeout: Duration::from_secs(60), // 1 минута таймаут
            max_missed_pings: 2,
        }
    }
}

pub struct HeartbeatSession {
    pub addr: SocketAddr,
    pub last_ping_received: Instant,
    pub last_ping_sent: Instant,
    pub missed_pings: u32,
    pub is_alive: bool,
}

pub struct HeartbeatManager {
    sessions: Arc<RwLock<HashMap<Vec<u8>, HeartbeatSession>>>,
    config: HeartbeatConfig,
    connection_manager: Arc<ConnectionManager>,
}

#[derive(Debug, Default)]
pub struct HeartbeatStats {
    pub total_sessions: usize,
    pub alive_sessions: usize,
    pub timed_out_sessions: usize,
}

impl HeartbeatManager {
    pub fn new(config: HeartbeatConfig, connection_manager: Arc<ConnectionManager>) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            config,
            connection_manager,
        }
    }

    pub async fn start(&self) {
        let sessions = Arc::clone(&self.sessions);
        let config = self.config.clone();
        let connection_manager = Arc::clone(&self.connection_manager);

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(10)); // Check every 10 seconds

            loop {
                interval.tick().await;
                Self::check_sessions(&sessions, &config, &connection_manager).await;
            }
        });
    }

    pub async fn session_exists(&self, session_id: &[u8]) -> bool {
        let sessions = self.sessions.read().await;
        sessions.contains_key(session_id)
    }

    // Метод для получения статистики heartbeat
    pub async fn get_heartbeat_stats(&self) -> HeartbeatStats {
        let sessions = self.sessions.read().await;
        let now = Instant::now();
        let mut stats = HeartbeatStats::default();

        for session in sessions.values() {
            stats.total_sessions += 1;
            if session.is_alive {
                stats.alive_sessions += 1;
            }
            if now.duration_since(session.last_ping_received) > self.config.timeout {
                stats.timed_out_sessions += 1;
            }
        }

        stats
    }

    async fn check_sessions(
        sessions: &Arc<RwLock<HashMap<Vec<u8>, HeartbeatSession>>>,
        config: &HeartbeatConfig,
        connection_manager: &Arc<ConnectionManager>,
    ) {
        let now = Instant::now();
        let mut to_remove = Vec::new();

        let sessions_read = sessions.read().await;
        for (session_id, session) in sessions_read.iter() {
            // Check if session timed out
            if now.duration_since(session.last_ping_received) > config.timeout {
                warn!(
                    "Heartbeat timeout for session {} from {}, closing connection. Last activity: {:?} ago",
                    hex::encode(session_id),
                    session.addr,
                    now.duration_since(session.last_ping_received)
                );
                to_remove.push(session_id.clone());
            }
        }
        drop(sessions_read);

        // Remove timed out sessions and force disconnect
        if !to_remove.is_empty() {
            for session_id in to_remove {
                // Принудительно разрываем соединение
                connection_manager.force_disconnect(&session_id).await;

                // Удаляем из heartbeat manager
                let mut sessions_write = sessions.write().await;
                sessions_write.remove(&session_id);
            }
        }
    }

    pub async fn register_session(&self, session_id: Vec<u8>, addr: SocketAddr) {
        let session_id_clone = session_id.clone();

        let mut sessions = self.sessions.write().await;
        sessions.insert(
            session_id,
            HeartbeatSession {
                addr,
                last_ping_received: Instant::now(),
                last_ping_sent: Instant::now(),
                missed_pings: 0,
                is_alive: true,
            },
        );

        info!("Heartbeat registered for session {} from {}",
          hex::encode(&session_id_clone), addr);
    }

    pub async fn unregister_session(&self, session_id: &[u8]) {
        let mut sessions = self.sessions.write().await;
        if sessions.remove(session_id).is_some() {
            info!("Heartbeat unregistered for session {}", hex::encode(session_id));
        }
    }

    pub async fn update_heartbeat_received(&self, session_id: &[u8]) -> bool {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.last_ping_received = Instant::now();
            session.missed_pings = 0;
            session.is_alive = true;
            info!(target: "heartbeat", "Heartbeat updated for session: {}", hex::encode(session_id));
            true
        } else {
            false
        }
    }

    pub async fn on_ping_sent(&self, session_id: &[u8]) -> bool {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.last_ping_sent = Instant::now();
            true
        } else {
            false
        }
    }

    pub async fn is_connection_alive(&self, session_id: &[u8]) -> bool {
        let sessions = self.sessions.read().await;
        sessions.get(session_id)
            .map(|session| session.is_alive)
            .unwrap_or(false)
    }

    pub async fn get_missed_pings(&self, session_id: &[u8]) -> u32 {
        let sessions = self.sessions.read().await;
        sessions.get(session_id)
            .map(|session| session.missed_pings)
            .unwrap_or(0)
    }
}