pub mod error;
pub mod server {
    pub mod security;
    pub mod connection_manager;
    pub mod gateway;
    pub mod session_manager;
    pub mod tcp_server;
}
pub mod crypto;
pub mod packets;
pub mod framing;
pub mod cache;

// Re-export часто используемых компонентов
pub use server::TcpServer;
pub use crypto::CryptoCtx;
pub use crypto::handshake::server_handshake;
pub use packets::encoder::build_encrypted_packet;
pub use packets::decoder::{decode_packet, PacketType};
pub use packets::processor::check_packet_type;
pub use framing::{read_frame, write_frame};
pub use dispatcher::Dispatcher;
pub use monitoring::run_metrics_server;
pub use cache::{SimpleCache, EnhancedCache};

pub use buffer::{PacketBuffer, PacketBuilder};
pub use error::{ProtocolError, ProtocolResult};