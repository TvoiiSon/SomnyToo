use std::sync::Arc;
use std::net::SocketAddr;
use tracing::{info, error};

use crate::core::protocol::crypto::key_manager::session_keys::SessionKeys;
use crate::core::protocol::packets::decoder::packet_parser::PacketType;
use crate::core::protocol::server::session_manager::SessionManager;

pub struct PacketProcessingResult {
    pub response: Vec<u8>,
    pub should_encrypt: bool,
}

pub struct PacketService {
    session_manager: Arc<SessionManager>,
}

impl PacketService {
    pub fn new(session_manager: Arc<SessionManager>) -> Self {
        Self {
            session_manager,
        }
    }

    pub async fn process_packet(
        &self,
        ctx: Arc<SessionKeys>,
        packet_type: PacketType,
        payload: Vec<u8>,
        client_ip: SocketAddr,
    ) -> Result<PacketProcessingResult, Box<dyn std::error::Error>> {
        info!("Processing packet type: {:?} from {}", packet_type, client_ip);

        info!("Payload size: {} bytes", payload.len());

        let response_data = match packet_type {
            PacketType::Ping => self.handle_ping(payload).await?,

            // System packets
            PacketType::Heartbeat => self.handle_heartbeat(&ctx.session_id, client_ip).await?,

            _ => self.handle_unknown_packet(packet_type).await?,
        };

        Ok(PacketProcessingResult {
            response: response_data,
            should_encrypt: true,
        })
    }

    async fn handle_ping(&self, _payload: Vec<u8>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        info!("Processing Ping packet");
        Ok(b"Ping...".to_vec())
    }

    async fn handle_heartbeat(
        &self,
        session_id: &[u8],
        client_ip: SocketAddr,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        info!("Processing heartbeat from {} session: {}",
              client_ip, hex::encode(session_id));

        // Update heartbeat status
        if self.session_manager.on_heartbeat_received(session_id).await {
            info!("Heartbeat confirmed for session: {}", hex::encode(session_id));
            Ok(b"Heartbeat acknowledged".to_vec())
        } else {
            error!("Heartbeat for unknown session: {}", hex::encode(session_id));
            Ok(b"Session not found".to_vec())
        }
    }

    async fn handle_unknown_packet(&self, packet_type: PacketType) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        error!("Unknown packet type: {:?}", packet_type);
        Ok(format!("Unknown packet type: {:?}", packet_type).into_bytes())
    }
}

// Исправленная реализация Clone
impl Clone for PacketService {
    fn clone(&self) -> Self {
        Self {
            session_manager: Arc::clone(&self.session_manager),
        }
    }
}