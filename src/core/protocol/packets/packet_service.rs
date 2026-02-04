use std::sync::Arc;
use std::net::SocketAddr;
use tracing::{info, error, debug};
use std::time::{Instant, Duration};

use crate::core::protocol::phantom_crypto::core::keys::PhantomSession;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;

pub struct PacketProcessingResult {
    pub response: Vec<u8>,
    pub should_encrypt: bool,
    pub packet_type: u8,
    pub priority: crate::core::protocol::batch_system::types::priority::Priority,
}

pub struct PhantomPacketService {
    phantom_session_manager: Arc<PhantomSessionManager>,
    heartbeat_manager: Arc<ConnectionHeartbeatManager>,
}

impl PhantomPacketService {
    pub fn new(
        phantom_session_manager: Arc<PhantomSessionManager>,
        heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    ) -> Self {
        Self {
            phantom_session_manager,
            heartbeat_manager,
        }
    }

    pub async fn process_packet(
        &self,
        session: Arc<PhantomSession>,
        packet_type: u8,
        payload: Vec<u8>,
        client_ip: SocketAddr,
    ) -> Result<PacketProcessingResult, Box<dyn std::error::Error + Send + Sync>> {
        let process_start = Instant::now();

        debug!("Processing phantom packet type: 0x{:02X} from {}, session: {}",
              packet_type, client_ip, hex::encode(session.session_id()));

        let (response_data, priority) = match packet_type {
            0x01 => {
                let response = self.handle_ping(payload, session.clone(), client_ip).await?;
                // PING/PONG пакеты имеют критический приоритет для быстрого ответа
                (response, crate::core::protocol::batch_system::types::priority::Priority::Critical)
            }
            0x10 => {
                let response = self.handle_heartbeat(session.session_id(), client_ip).await?;
                // Heartbeat пакеты имеют высокий приоритет
                (response, crate::core::protocol::batch_system::types::priority::Priority::High)
            }
            _ => {
                let response = self.handle_unknown_packet(packet_type, payload, session.clone(), client_ip).await?;
                // Неизвестные пакеты имеют нормальный приоритет
                (response, crate::core::protocol::batch_system::types::priority::Priority::Normal)
            }
        };

        let total_time = process_start.elapsed();
        if total_time > Duration::from_millis(5) {
            debug!("PhantomPacketService total processing time: {:?} for 0x{:02X}",
                  total_time, packet_type);
        }

        Ok(PacketProcessingResult {
            response: response_data,
            should_encrypt: true,
            packet_type,
            priority,
        })
    }

    async fn handle_ping(
        &self,
        payload: Vec<u8>,
        session: Arc<PhantomSession>,
        client_ip: SocketAddr,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let start = Instant::now();

        info!("👻 Ping packet received from {}: {} ({} bytes)",
        client_ip, String::from_utf8_lossy(&payload), payload.len());

        self.heartbeat_manager.send_custom_alert(
            crate::core::monitoring::unified_monitor::AlertLevel::Info,
            "phantom_packet",
            &format!("Ping packet received from {} for session {}",
                     client_ip, hex::encode(session.session_id()))
        ).await;

        // ВАЖНО: Возвращаем PONG как plaintext payload
        // Клиент ожидает ответ с тем же packet_type (0x01), но с другим содержимым
        let result = b"PONG".to_vec();

        let elapsed = start.elapsed();

        if elapsed > Duration::from_millis(1) {
            debug!("Ping handle took {:?}", elapsed);
        }

        Ok(result)
    }

    async fn handle_heartbeat(
        &self,
        session_id: &[u8],
        client_ip: SocketAddr,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let start = Instant::now();

        debug!("Processing phantom heartbeat from {} session: {}",
              client_ip, hex::encode(session_id));

        let heartbeat_result = if self.phantom_session_manager.on_heartbeat_received(session_id).await {
            debug!("Heartbeat confirmed for phantom session: {}", hex::encode(session_id));
            b"Heartbeat acknowledged".to_vec()
        } else {
            error!("Heartbeat for unknown phantom session: {}", hex::encode(session_id));
            b"Session not found".to_vec()
        };

        let total_time = start.elapsed();
        debug!("Phantom heartbeat processing: {:?}", total_time);

        Ok(heartbeat_result)
    }

    async fn handle_unknown_packet(
        &self,
        packet_type: u8,
        _payload: Vec<u8>,
        session: Arc<PhantomSession>,
        client_ip: SocketAddr,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        error!("Unknown phantom packet type: 0x{:02X} from {}, session: {}",
               packet_type, client_ip, hex::encode(session.session_id()));

        self.heartbeat_manager.send_custom_alert(
            crate::core::monitoring::unified_monitor::AlertLevel::Warning,
            "unknown_packet",
            &format!("Unknown packet type 0x{:02X} from {} for session {}",
                     packet_type, client_ip, hex::encode(session.session_id()))
        ).await;

        Ok(format!("Unknown phantom packet type: 0x{:02X}", packet_type).into_bytes())
    }
}

impl Clone for PhantomPacketService {
    fn clone(&self) -> Self {
        Self {
            phantom_session_manager: Arc::clone(&self.phantom_session_manager),
            heartbeat_manager: Arc::clone(&self.heartbeat_manager),
        }
    }
}