use aes_gcm::{
    aead::{Aead, generic_array::GenericArray}
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use constant_time_eq::constant_time_eq;
use tracing::{info, warn, debug, trace};

use crate::core::protocol::crypto::key_manager::session_keys::SessionKeys;
use crate::core::protocol::error::{ProtocolError, CryptoError, ProtocolResult};

pub const HEADER_MAGIC: [u8; 2] = [0xAB, 0xCD];
const MAX_PAYLOAD_SIZE: usize = 1 << 20;
const SIGNATURE_SIZE: usize = 32;
const NONCE_SIZE: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PacketType {
    Ping,
    Heartbeat,
    Unknown(u8),
}

impl From<u8> for PacketType {
    fn from(t: u8) -> Self {
        match t {
            0x01 => PacketType::Ping,
            0x10 => PacketType::Heartbeat,
            x => PacketType::Unknown(x),
        }
    }
}

#[derive(Debug, Clone)]
pub enum DecodeError {
    InvalidLength,
    InvalidMagic,
    InvalidSignature,
    DecryptionFailed,
    InvalidPacketType,
}

type HmacSha256 = Hmac<Sha256>;

pub struct PacketParser;

impl PacketParser {
    pub fn decode_packet(ctx: &SessionKeys, data: &[u8]) -> ProtocolResult<(u8, Vec<u8>)> {
        let session_id_hex = hex::encode(&ctx.session_id);
        info!(
            target: "packet_decoder",
            "Starting packet decode - session_id: {}, data_len: {}",
            session_id_hex,
            data.len()
        );

        let minimal = 2 + 2 + 1 + NONCE_SIZE + 16 + SIGNATURE_SIZE;

        if data.len() < minimal {
            let error = format!("Packet too short: {} bytes, expected at least {}", data.len(), minimal);
            warn!(
                target: "packet_decoder",
                "Decode failed - {}",
                error
            );
            return Err(ProtocolError::MalformedPacket {
                details: error
            });
        }

        if !constant_time_eq(&data[0..2], &HEADER_MAGIC) {
            let received_magic = hex::encode(&data[0..2]);
            let expected_magic = hex::encode(&HEADER_MAGIC);
            warn!(
                target: "packet_decoder",
                "Invalid magic bytes - received: {}, expected: {}",
                received_magic,
                expected_magic
            );
            return Err(ProtocolError::MalformedPacket {
                details: "Invalid magic bytes".to_string()
            });
        }

        let length = u16::from_be_bytes([data[2], data[3]]) as usize;
        debug!(
            target: "packet_decoder",
            "Packet header parsed - magic: {}, length: {}",
            hex::encode(&HEADER_MAGIC),
            length
        );

        if length < 1 + NONCE_SIZE + 16 + SIGNATURE_SIZE {
            let error = format!("Invalid length field: {} bytes", length);
            warn!(
                target: "packet_decoder",
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
                target: "packet_decoder",
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
                target: "packet_decoder",
                "Decode failed - {}",
                error
            );
            return Err(ProtocolError::MalformedPacket {
                details: error
            });
        }

        let packet_type_raw = data[4];
        debug!(
            target: "packet_decoder",
            "Packet type: 0x{:02X}",
            packet_type_raw
        );

        if packet_type_raw == 0xFF {
            warn!(
                target: "packet_decoder",
                "Reserved packet type rejected - 0xFF"
            );
            return Err(ProtocolError::MalformedPacket {
                details: "Reserved packet type".to_string()
            });
        }

        let hmac_start = 4 + length - SIGNATURE_SIZE;
        if hmac_start <= 4 || hmac_start + SIGNATURE_SIZE > data.len() {
            let error = format!("Invalid HMAC position: {}, data_len: {}", hmac_start, data.len());
                warn!(
                    target: "packet_decoder",
                    "Decode failed - {}",
                    error
                );
            return Err(ProtocolError::MalformedPacket {
                details: error
            });
        }

        let signed_part = &data[0..hmac_start];
        let signature = &data[hmac_start..hmac_start + SIGNATURE_SIZE];

        trace!(
            target: "packet_decoder",
            "HMAC verification - signed_part_len: {}, signature: {}",
            signed_part.len(),
            hex::encode(signature)
        );

        let hmac_start_time = std::time::Instant::now();
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&ctx.sign_key)
            .map_err(|_| {
                let error = ProtocolError::Crypto {
                    source: CryptoError::InvalidKeyLength {
                        expected: 32,
                        actual: ctx.sign_key.len()
                    }
                };
                warn!(
                    target: "packet_decoder",
                    "HMAC key error - expected: 32, actual: {}",
                    ctx.sign_key.len()
                );
                error
            })?;

        mac.update(signed_part);
        let computed_tag = mac.finalize().into_bytes();
        let hmac_duration = hmac_start_time.elapsed();

        if !constant_time_eq(&computed_tag, signature) {
            warn!(
                target: "packet_decoder",
                "HMAC verification failed - computed: {}, received: {}",
                hex::encode(&computed_tag),
                hex::encode(signature)
            );
            let error = ProtocolError::Crypto {
                source: CryptoError::HmacVerificationFailed
            }.log();
            return Err(error);
        }

        debug!(
            target: "packet_decoder",
            "HMAC verified successfully - duration: {:?}",
            hmac_duration
        );

        let header_with_type_len = 2 + 2 + 1;
        if signed_part.len() < header_with_type_len + NONCE_SIZE + 16 {
            let error = format!(
                "Invalid payload structure: signed_part_len={}, required_at_least={}",
                signed_part.len(),
                header_with_type_len + NONCE_SIZE + 16
            );
            warn!(
                target: "packet_decoder",
                "Decode failed - {}",
                error
            );
            return Err(ProtocolError::MalformedPacket {
                details: error
            });
        }

        let payload = &signed_part[header_with_type_len..];
        let nonce_bytes = &payload[..NONCE_SIZE];
        let ciphertext = &payload[NONCE_SIZE..];

        trace!(
            target: "packet_decoder",
            "Crypto components - nonce: {}, ciphertext_len: {}",
            hex::encode(nonce_bytes),
            ciphertext.len()
        );

        let aad = &signed_part[0..header_with_type_len];
        let nonce = GenericArray::from_slice(nonce_bytes);

        let decryption_start_time = std::time::Instant::now();
        use aes_gcm::aead::Payload;
        let plaintext = ctx.aead_cipher
            .decrypt(
                nonce,
                Payload {
                    msg: ciphertext,
                    aad,
                }
            )
            .map_err(|e| {
                let error_details = e.to_string();
                    warn!(
                    target: "packet_decoder",
                    "Decryption failed - reason: {}, nonce: {}, ciphertext_len: {}",
                    error_details,
                    hex::encode(nonce_bytes),
                    ciphertext.len()
                );
                ProtocolError::Crypto {
                    source: CryptoError::DecryptionFailed {
                        reason: error_details
                    }
                }
            })?;
        let decryption_duration = decryption_start_time.elapsed();

        info!(
            target: "packet_decoder",
            "Packet decoded successfully - type: 0x{:02X}, plaintext_len: {}, total_duration: {:?} (HMAC: {:?}, decrypt: {:?}), session_id: {}",
            packet_type_raw,
            plaintext.len(),
            hmac_duration + decryption_duration,
            hmac_duration,
            decryption_duration,
            session_id_hex
        );

        debug!(
            target: "packet_decoder",
            "Packet breakdown - total: {} bytes, header: {} bytes, nonce: {} bytes, ciphertext: {} bytes, hmac: {} bytes, plaintext: {} bytes",
            data.len(),
            header_with_type_len,
            NONCE_SIZE,
            ciphertext.len(),
            SIGNATURE_SIZE,
            plaintext.len()
        );

        Ok((packet_type_raw, plaintext))
    }
}
