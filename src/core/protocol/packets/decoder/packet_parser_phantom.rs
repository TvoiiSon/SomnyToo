use tracing::{info, warn, debug};

use crate::core::protocol::phantom_crypto::{
    keys::PhantomSession,
    packet::{PhantomPacketProcessor, HEADER_MAGIC},
};
use crate::core::protocol::error::{ProtocolResult, ProtocolError};

pub const MAX_PAYLOAD_SIZE: usize = 1 << 20; // 1 MB

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PhantomPacketType {
    Ping,
    Heartbeat,
    Data,
    Control,
    Unknown(u8),
}

impl From<u8> for PhantomPacketType {
    fn from(t: u8) -> Self {
        match t {
            0x01 => PhantomPacketType::Ping,
            0x10 => PhantomPacketType::Heartbeat,
            0x20 => PhantomPacketType::Data,
            0x30 => PhantomPacketType::Control,
            x => PhantomPacketType::Unknown(x),
        }
    }
}

pub struct PhantomPacketParser {
    processor: PhantomPacketProcessor,
}

impl PhantomPacketParser {
    pub fn new() -> Self {
        Self {
            processor: PhantomPacketProcessor::new(),
        }
    }

    pub fn decode_packet(
        &self,
        session: &PhantomSession,
        data: &[u8],
    ) -> ProtocolResult<(u8, Vec<u8>)> {
        let session_id_hex = hex::encode(session.session_id());
        info!(
            target: "phantom_packet_decoder",
            "Starting phantom packet decode - session_id: {}, data_len: {}",
            session_id_hex,
            data.len()
        );

        // Минимальная проверка заголовка
        if data.len() < 4 {
            let error = format!("Packet too short: {} bytes", data.len());
            warn!(
                target: "phantom_packet_decoder",
                "Decode failed - {}",
                error
            );
            return Err(ProtocolError::MalformedPacket {
                details: error
            });
        }

        // Проверка magic байтов
        if !constant_time_eq::constant_time_eq(&data[0..2], &HEADER_MAGIC) {
            let received_magic = hex::encode(&data[0..2]);
            let expected_magic = hex::encode(&HEADER_MAGIC);
            warn!(
                target: "phantom_packet_decoder",
                "Invalid magic bytes - received: {}, expected: {}",
                received_magic,
                expected_magic
            );
            return Err(ProtocolError::MalformedPacket {
                details: "Invalid magic bytes".to_string()
            });
        }

        // Проверка длины
        let length = u16::from_be_bytes([data[2], data[3]]) as usize;

        if length < 16 + 8 + 8 + 1 + 12 + 32 {
            let error = format!("Invalid length field: {} bytes", length);
            warn!(
                target: "phantom_packet_decoder",
                "Decode failed - {}",
                error
            );
            return Err(ProtocolError::MalformedPacket {
                details: error
            });
        }

        if length > MAX_PAYLOAD_SIZE {
            let error = format!("Packet too large: {} bytes, max: {}", length, MAX_PAYLOAD_SIZE);
            warn!(
                target: "phantom_packet_decoder",
                "Decode failed - {}",
                error
            );
            return Err(ProtocolError::MalformedPacket {
                details: error
            });
        }

        let expected_total = 4 + length;
        if data.len() != expected_total {
            let error = format!("Length mismatch: expected {}, got {}", expected_total, data.len());
            warn!(
                target: "phantom_packet_decoder",
                "Decode failed - {}",
                error
            );
            return Err(ProtocolError::MalformedPacket {
                details: error
            });
        }

        // Используем процессор фантомных пакетов
        let result = self.processor.process_incoming(data, session);

        match result {
            Ok((packet_type, plaintext)) => {
                debug!(
                    target: "phantom_packet_decoder",
                    "Successfully decoded phantom packet type: 0x{:02X}, plaintext: {} bytes",
                    packet_type,
                    plaintext.len()
                );

                Ok((packet_type, plaintext))
            }
            Err(e) => {
                warn!(
                    target: "phantom_packet_decoder",
                    "Phantom packet decode failed: {}",
                    e
                );
                Err(e)
            }
        }
    }

    /// Быстрая проверка, является ли data валидным пакетом (без полного парсинга)
    pub fn is_likely_valid_packet(data: &[u8]) -> bool {
        if data.len() < 4 {
            return false;
        }

        // Проверяем только magic байты
        data[0..2] == HEADER_MAGIC
    }

    /// Извлекает session_id из пакета без полного парсинга
    pub fn extract_session_id(data: &[u8]) -> Option<[u8; 16]> {
        if data.len() < 4 + 16 {
            return None;
        }

        data[4..20].try_into().ok()
    }
}

impl Default for PhantomPacketParser {
    fn default() -> Self {
        Self::new()
    }
}