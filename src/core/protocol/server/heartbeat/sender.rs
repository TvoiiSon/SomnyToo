use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tracing::{info, warn, debug};

use crate::core::protocol::server::heartbeat::manager::{HeartbeatManager, HeartbeatSessionInfo};

pub struct HeartbeatSender {
    heartbeat_manager: Arc<HeartbeatManager>,
}

impl HeartbeatSender {
    pub fn new(heartbeat_manager: Arc<HeartbeatManager>) -> Self {
        Self { heartbeat_manager }
    }

    pub async fn start(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(30)); // Send heartbeat every 30 seconds

            loop {
                interval.tick().await;
                self.send_heartbeats().await;
            }
        });
    }

    async fn send_heartbeats(&self) {
        debug!("Heartbeat sender: checking active sessions");

        // Получаем активные сессии через heartbeat_manager
        let active_sessions = self.heartbeat_manager.get_active_sessions().await;

        for session_info in active_sessions {
            let session_id = session_info.session_id.clone();

            // Проверяем, жива ли сессия перед отправкой heartbeat
            if self.heartbeat_manager.is_connection_alive(&session_id).await {
                // Проверяем, нужно ли отправлять heartbeat
                if self.heartbeat_manager.should_send_heartbeat(&session_id).await {
                    if let Err(e) = self.send_heartbeat(session_info).await {
                        warn!("Failed to send heartbeat to session {}: {}",
                            hex::encode(&session_id), e);
                    }
                } else {
                    debug!("Heartbeat not needed for session {} yet",
                        hex::encode(&session_id));
                }
            } else {
                // Сессия мертва, убираем из активных
                info!("Removing dead session from heartbeat: {}", hex::encode(&session_id));
                self.heartbeat_manager.force_remove_session(&session_id).await;
            }
        }
    }

    async fn send_heartbeat(&self, session_info: HeartbeatSessionInfo)
                            -> Result<(), anyhow::Error> // Изменяем тип возвращаемого значения
    {
        let session_id = session_info.session_id;

        debug!("Sending heartbeat to session: {} from {}",
            hex::encode(&session_id), session_info.addr);

        // Отправляем heartbeat через heartbeat_manager
        self.heartbeat_manager.send_heartbeat(session_id).await
    }

    /// Проверяет, нужно ли отправлять heartbeat для сессии
    pub async fn should_send_heartbeat(&self, session_id: &[u8]) -> bool {
        self.heartbeat_manager.should_send_heartbeat(session_id).await
    }
}