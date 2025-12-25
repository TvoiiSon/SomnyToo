use std::sync::Arc;
use std::time::Duration;
use tokio::time::{interval};
use tracing::{info, warn};

use crate::core::protocol::packets::encoder::packet_builder::PacketBuilder;
use crate::core::protocol::server::session_manager::SessionManager;
use crate::core::protocol::crypto::key_manager::session_keys::SessionKeys;

pub struct HeartbeatSender {
    session_manager: Arc<SessionManager>,
}

impl HeartbeatSender {
    pub fn new(session_manager: Arc<SessionManager>) -> Self {
        Self { session_manager }
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
        info!("Heartbeat sender: checking active sessions");

        let active_sessions = self.session_manager.get_active_sessions().await;

        for session_keys in active_sessions {
            // Проверяем, жива ли сессия перед отправкой heartbeat
            if self.session_manager.is_connection_alive(&session_keys.session_id).await {
                if self.session_manager.should_send_heartbeat(&session_keys.session_id).await {
                    if let Err(e) = self.send_heartbeat_to_session(&session_keys).await {
                        warn!("Failed to send heartbeat to session {}: {}",
                          hex::encode(&session_keys.session_id), e);
                    }
                }
            } else {
                // Сессия мертва, убираем из активных
                info!("Removing dead session from heartbeat: {}", hex::encode(&session_keys.session_id));
                self.session_manager.force_remove_session(&session_keys.session_id).await;
            }
        }
    }

    // Добавляем метод для проверки необходимости отправки heartbeat
    pub async fn should_send_heartbeat(&self, session_id: &[u8]) -> bool {
        self.session_manager.should_send_heartbeat(session_id).await
    }

    // This would be called when we have a specific connection to send heartbeats to
    pub async fn send_heartbeat_to_session(&self, session_keys: &SessionKeys) -> Result<(), Box<dyn std::error::Error>> {
        // Build heartbeat packet
        let _heartbeat_packet = PacketBuilder::build_encrypted_packet(
            session_keys,
            0x11, // Heartbeat packet type
            b"ping", // Heartbeat payload
        ).await;

        // In a real implementation, we would send this packet through the connection
        // For now, we'll just log it
        info!("Would send heartbeat to session: {}", hex::encode(&session_keys.session_id));

        Ok(())
    }
}