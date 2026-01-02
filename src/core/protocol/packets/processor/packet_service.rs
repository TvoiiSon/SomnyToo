use std::sync::Arc;
use std::net::SocketAddr;
use tracing::{info, error, warn, debug};
use std::time::{Instant, Duration};

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
        let process_start = Instant::now();
        info!("Processing packet type: {:?} from {}, session: {}",
              packet_type, client_ip, hex::encode(&ctx.session_id));

        info!("Payload size: {} bytes", payload.len());

        let response_data = match packet_type {
            PacketType::Ping => {
                let ping_start = Instant::now();
                let result = self.handle_ping(payload).await?;
                let ping_time = ping_start.elapsed();
                debug!("Ping processing took {:?}", ping_time);
                result
            }

            // System packets
            PacketType::Heartbeat => {
                let heartbeat_start = Instant::now();
                let result = self.handle_heartbeat(&ctx.session_id, client_ip).await?;
                let heartbeat_time = heartbeat_start.elapsed();
                debug!("Heartbeat processing took {:?}", heartbeat_time);
                result
            }

            _ => {
                let unknown_start = Instant::now();
                let result = self.handle_unknown_packet(packet_type).await?;
                let unknown_time = unknown_start.elapsed();
                warn!("Unknown packet processing took {:?}", unknown_time);
                result
            }
        };

        let total_time = process_start.elapsed();
        if total_time > Duration::from_millis(5) {
            info!("PacketService total processing time: {:?} for {:?}", total_time, packet_type);
        }

        Ok(PacketProcessingResult {
            response: response_data,
            should_encrypt: true,
        })
    }

    async fn handle_ping(&self, _payload: Vec<u8>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let start = Instant::now();
        info!("Processing Ping packet");
        let result = b"".to_vec();
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
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let start = Instant::now();
        info!("Processing heartbeat from {} session: {}", client_ip, hex::encode(session_id));

        // Update heartbeat status
        let heartbeat_start = Instant::now();
        let heartbeat_result = if self.session_manager.on_heartbeat_received(session_id).await {
            info!("Heartbeat confirmed for session: {}", hex::encode(session_id));
            b"Heartbeat acknowledged".to_vec()
        } else {
            error!("Heartbeat for unknown session: {}", hex::encode(session_id));
            b"Session not found".to_vec()
        };
        let heartbeat_time = heartbeat_start.elapsed();

        let total_time = start.elapsed();
        debug!("Heartbeat processing - session update: {:?}, total: {:?}",
               heartbeat_time, total_time);

        Ok(heartbeat_result)
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