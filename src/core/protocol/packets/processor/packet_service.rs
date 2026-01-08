use std::sync::Arc;
use std::net::SocketAddr;
use tracing::{info, error, warn, debug};
use std::time::{Instant, Duration};

// Заменяем SessionKeys на PhantomSession
use crate::core::protocol::phantom_crypto::core::keys::PhantomSession;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;

pub struct PacketProcessingResult {
    pub response: Vec<u8>,
    pub should_encrypt: bool,
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
    ) -> Result<PacketProcessingResult, Box<dyn std::error::Error + Send + Sync>> { // Добавляем Send + Sync
        let process_start = Instant::now();
        info!("Processing phantom packet type: 0x{:02X} from {}, session: {}",
              packet_type, client_ip, hex::encode(session.session_id()));

        info!("Payload size: {} bytes", payload.len());

        let response_data = match packet_type {
            0x01 => { // Ping packet
                let ping_start = Instant::now();
                let result = self.handle_ping(payload, session.clone(), client_ip).await?;
                let ping_time = ping_start.elapsed();
                debug!("Ping processing took {:?}", ping_time);
                result
            }
            0x10 => { // Heartbeat packet
                // Heartbeat уже обрабатывается в connection_manager_phantom
                let heartbeat_start = Instant::now();
                let result = self.handle_heartbeat(session.session_id(), client_ip).await?;
                let heartbeat_time = heartbeat_start.elapsed();
                debug!("Heartbeat processing took {:?}", heartbeat_time);
                result
            }
            _ => {
                let unknown_start = Instant::now();
                let result = self.handle_unknown_packet(packet_type, payload, session.clone(), client_ip).await?;
                let unknown_time = unknown_start.elapsed();
                warn!("Unknown packet processing took {:?}", unknown_time);
                result
            }
        };

        let total_time = process_start.elapsed();
        if total_time > Duration::from_millis(5) {
            info!("PhantomPacketService total processing time: {:?} for 0x{:02X}",
                  total_time, packet_type);
        }

        Ok(PacketProcessingResult {
            response: response_data,
            should_encrypt: true,  // Всегда шифруем в фантомной системе
        })
    }

    async fn handle_ping(
        &self,
        payload: Vec<u8>,
        session: Arc<PhantomSession>,
        client_ip: SocketAddr,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> { // Добавляем Send + Sync
        let start = Instant::now();
        info!("👻 Ping packet received from {}: {} ({} bytes)",
              client_ip, String::from_utf8_lossy(&payload), payload.len());

        // Отправляем алерт в heartbeat_manager
        self.heartbeat_manager.send_custom_alert(
            crate::core::monitoring::unified_monitor::AlertLevel::Info,
            "phantom_packet",
            &format!("Ping packet received from {} for session {}",
                     client_ip, hex::encode(session.session_id()))
        ).await;

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
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> { // Добавляем Send + Sync
        let start = Instant::now();
        info!("Processing phantom heartbeat from {} session: {}",
              client_ip, hex::encode(session_id));

        // Update heartbeat status
        let heartbeat_start = Instant::now();
        let heartbeat_result = if self.phantom_session_manager.on_heartbeat_received(session_id).await {
            info!("Heartbeat confirmed for phantom session: {}", hex::encode(session_id));
            b"Heartbeat acknowledged".to_vec()
        } else {
            error!("Heartbeat for unknown phantom session: {}", hex::encode(session_id));
            b"Session not found".to_vec()
        };
        let heartbeat_time = heartbeat_start.elapsed();

        let total_time = start.elapsed();
        debug!("Phantom heartbeat processing - session update: {:?}, total: {:?}",
               heartbeat_time, total_time);

        Ok(heartbeat_result)
    }

    async fn handle_unknown_packet(
        &self,
        packet_type: u8,
        _payload: Vec<u8>,
        session: Arc<PhantomSession>,
        client_ip: SocketAddr,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> { // Добавляем Send + Sync
        error!("Unknown phantom packet type: 0x{:02X} from {}, session: {}",
               packet_type, client_ip, hex::encode(session.session_id()));

        // Отправляем алерт о неизвестном пакете
        self.heartbeat_manager.send_custom_alert(
            crate::core::monitoring::unified_monitor::AlertLevel::Warning,
            "unknown_packet",
            &format!("Unknown packet type 0x{:02X} from {} for session {}",
                     packet_type, client_ip, hex::encode(session.session_id()))
        ).await;

        Ok(format!("Unknown phantom packet type: 0x{:02X}", packet_type).into_bytes())
    }
}

// Исправленная реализация Clone
impl Clone for PhantomPacketService {
    fn clone(&self) -> Self {
        Self {
            phantom_session_manager: Arc::clone(&self.phantom_session_manager),
            heartbeat_manager: Arc::clone(&self.heartbeat_manager),
        }
    }
}