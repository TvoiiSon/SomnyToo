use std::time::Instant;
use aes_gcm::{Aes256Gcm, KeyInit, aead::{Aead, generic_array::GenericArray}};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use rand_core::{OsRng, RngCore};
use constant_time_eq::constant_time_eq;
use tracing::{info, warn, debug};

use super::keys::{PhantomSession, PhantomOperationKey};
use crate::core::protocol::error::{ProtocolResult, ProtocolError, CryptoError};

type HmacSha256 = Hmac<Sha256>;

/// Константы пакетов
pub const HEADER_MAGIC: [u8; 2] = [0xAB, 0xCE];
const NONCE_SIZE: usize = 12;
const SIGNATURE_SIZE: usize = 32;
const MAX_PAYLOAD_SIZE: usize = 1 << 20; // 1 MB

/// Пакет с фантомной криптографией
pub struct PhantomPacket {
    pub session_id: [u8; 16],
    pub sequence: u64,
    pub timestamp: u64,
    pub packet_type: u8,
    pub ciphertext: Vec<u8>,
    pub signature: [u8; 32],
}

impl PhantomPacket {
    /// Создает новый пакет для отправки
    pub fn create(
        session: &PhantomSession,
        packet_type: u8,
        plaintext: &[u8],
    ) -> ProtocolResult<Self> {
        let start = Instant::now();

        info!("Creating phantom packet: type=0x{:02X}, size={} bytes", packet_type, plaintext.len());

        // 1. Генерируем операционный ключ для шифрования
        let operation_key = session.generate_operation_key("encrypt");

        // 2. Генерируем nonce
        let mut nonce = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce);

        debug!("Generated nonce for encryption: {}", hex::encode(&nonce));

        // 3. Шифруем данные
        let cipher = Aes256Gcm::new_from_slice(operation_key.as_bytes())
            .map_err(|e| ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: format!("Failed to create cipher: {}", e)
                }
            })?;

        let payload_nonce = GenericArray::from_slice(&nonce);
        let ciphertext = cipher.encrypt(payload_nonce, plaintext)
            .map_err(|e| ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: format!("Encryption failed: {}", e)
                }
            })?;

        debug!("Encrypted data length: {} bytes", ciphertext.len());

        // 4. Объединяем nonce + ciphertext для передачи
        let mut full_ciphertext = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
        full_ciphertext.extend_from_slice(&nonce);
        full_ciphertext.extend_from_slice(&ciphertext);

        // 5. Создаем подпись (передаем nonce отдельно)
        let signature = Self::create_signature(
            session,
            &operation_key,
            packet_type,
            &nonce,
            &ciphertext,
        )?;

        // 6. Получаем метаданные
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let sequence = operation_key.sequence;

        let elapsed = start.elapsed();

        // Замер времени создания пакета
        #[cfg(feature = "metrics")]
        metrics::histogram!("phantom.packet.creation_time", elapsed.as_micros() as f64);

        debug!(
            "Phantom packet created in {:?}: type=0x{:02X}, size={} bytes",
            elapsed, packet_type, full_ciphertext.len()
        );

        Ok(Self {
            session_id: *session.session_id(),
            sequence,
            timestamp,
            packet_type,
            ciphertext: full_ciphertext,
            signature,
        })
    }

    /// Создает подпись пакета
    fn create_signature(
        session: &PhantomSession,
        sign_key: &PhantomOperationKey,
        packet_type: u8,
        nonce: &[u8; NONCE_SIZE],
        encrypted_data: &[u8],
    ) -> ProtocolResult<[u8; 32]> {
        let mut data_to_sign = Vec::new();

        data_to_sign.extend_from_slice(session.session_id());
        data_to_sign.extend_from_slice(&sign_key.sequence.to_be_bytes());
        data_to_sign.push(packet_type);
        data_to_sign.extend_from_slice(nonce);
        data_to_sign.extend_from_slice(encrypted_data);

        debug!(
            "Creating signature for session {} sequence {}: data_len={}, nonce={}, encrypted_data_len={}",
            hex::encode(session.session_id()),
            sign_key.sequence,
            data_to_sign.len(),
            hex::encode(nonce),
            encrypted_data.len()
        );

        let mut mac: HmacSha256 = Mac::new_from_slice(sign_key.as_bytes())
            .map_err(|_| ProtocolError::Crypto {
                source: CryptoError::InvalidKeyLength {
                    expected: 32,
                    actual: sign_key.as_bytes().len()
                }
            })?;

        mac.update(&data_to_sign);
        let signature_bytes = mac.finalize().into_bytes();

        let signature: [u8; 32] = signature_bytes.as_slice().try_into()
            .map_err(|_| ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Failed to convert signature to array".to_string()
                }
            })?;

        debug!("Signature created: {}", hex::encode(&signature));

        Ok(signature)
    }

    /// Кодирует пакет в байты для отправки
    pub fn encode(&self) -> Vec<u8> {
        let mut buffer = Vec::new();

        buffer.extend_from_slice(&HEADER_MAGIC);

        let total_len = 16 + 8 + 8 + 1 + self.ciphertext.len() + SIGNATURE_SIZE;
        buffer.extend_from_slice(&(total_len as u16).to_be_bytes());

        buffer.extend_from_slice(&self.session_id);
        buffer.extend_from_slice(&self.sequence.to_be_bytes());
        buffer.extend_from_slice(&self.timestamp.to_be_bytes());
        buffer.push(self.packet_type);
        buffer.extend_from_slice(&self.ciphertext);
        buffer.extend_from_slice(&self.signature);

        buffer
    }

    /// Декодирует пакет из байтов
    pub fn decode(data: &[u8]) -> ProtocolResult<Self> {
        let start = Instant::now();

        if data.len() < 4 {
            return Err(ProtocolError::MalformedPacket {
                details: "Packet too short".to_string()
            });
        }

        if !constant_time_eq(&data[0..2], &HEADER_MAGIC) {
            return Err(ProtocolError::MalformedPacket {
                details: "Invalid magic bytes".to_string()
            });
        }

        let length = u16::from_be_bytes([data[2], data[3]]) as usize;
        let min_length = 16 + 8 + 8 + 1 + NONCE_SIZE + SIGNATURE_SIZE;

        if length < min_length {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Invalid length: {} (min: {})", length, min_length)
            });
        }

        let expected_total = 4 + length;
        if data.len() != expected_total {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Length mismatch: expected {}, got {}", expected_total, data.len())
            });
        }

        let mut offset = 4;

        let session_id: [u8; 16] = data[offset..offset + 16].try_into()
            .map_err(|_| ProtocolError::MalformedPacket {
                details: "Invalid session id".to_string()
            })?;
        offset += 16;

        let sequence = u64::from_be_bytes(
            data[offset..offset + 8].try_into()
                .map_err(|_| ProtocolError::MalformedPacket {
                    details: "Invalid sequence".to_string()
                })?
        );
        offset += 8;

        let timestamp = u64::from_be_bytes(
            data[offset..offset + 8].try_into()
                .map_err(|_| ProtocolError::MalformedPacket {
                    details: "Invalid timestamp".to_string()
                })?
        );
        offset += 8;

        let packet_type = data[offset];
        offset += 1;

        let remaining_data = &data[offset..];
        if remaining_data.len() < NONCE_SIZE + SIGNATURE_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Not enough data for ciphertext and signature: {} bytes", remaining_data.len())
            });
        }

        let ciphertext_len = remaining_data.len() - SIGNATURE_SIZE;
        let ciphertext = remaining_data[..ciphertext_len].to_vec();
        let signature: [u8; 32] = remaining_data[ciphertext_len..].try_into()
            .map_err(|_| ProtocolError::MalformedPacket {
                details: "Invalid signature".to_string()
            })?;

        if ciphertext.len() < NONCE_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Ciphertext too short for nonce: {} bytes", ciphertext.len())
            });
        }

        let current_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let packet_timestamp_secs = timestamp / 1000;
        if current_timestamp.abs_diff(packet_timestamp_secs) > 30 {
            warn!("Packet timestamp out of range: {}", packet_timestamp_secs);
            return Err(ProtocolError::MalformedPacket {
                details: "Timestamp out of range".to_string()
            });
        }

        let elapsed = start.elapsed();
        debug!(
            "Phantom packet decoded in {:?}: session={}, sequence={}, type=0x{:02X}, ciphertext={} bytes",
            elapsed,
            hex::encode(session_id),
            sequence,
            packet_type,
            ciphertext.len()
        );

        Ok(Self {
            session_id,
            sequence,
            timestamp,
            packet_type,
            ciphertext,
            signature,
        })
    }

    /// Расшифровывает содержимое пакета
    pub fn decrypt(
        &self,
        session: &PhantomSession,
    ) -> ProtocolResult<Vec<u8>> {
        let start = Instant::now();

        debug!("Decrypting phantom packet: session={}, sequence={}",
               hex::encode(self.session_id), self.sequence);

        if !constant_time_eq(&self.session_id, session.session_id()) {
            return Err(ProtocolError::AuthenticationFailed {
                reason: "Session ID mismatch".to_string()
            });
        }

        self.verify_signature(session)?;

        if self.ciphertext.len() < NONCE_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: "Ciphertext too short".to_string()
            });
        }

        let nonce = &self.ciphertext[..NONCE_SIZE];
        let encrypted_data = &self.ciphertext[NONCE_SIZE..];

        debug!("Extracted nonce: {}, encrypted_data length: {} bytes",
               hex::encode(nonce), encrypted_data.len());

        // ВАЖНО: используем "encrypt" тип (а не "decrypt"), чтобы ключ совпадал с клиентом!
        let decrypt_key = session.generate_operation_key_for_sequence(self.sequence, "encrypt");

        debug!("SERVER: decrypt_key (first 8 bytes): {}",
               hex::encode(&decrypt_key.as_bytes()[..8]));
        debug!("SERVER: Full encrypted_data received: {}",
               hex::encode(encrypted_data));

        let cipher = Aes256Gcm::new_from_slice(decrypt_key.as_bytes())
            .map_err(|e| ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: format!("Failed to create cipher: {}", e)
                }
            })?;

        let payload_nonce = GenericArray::from_slice(nonce);
        let plaintext = cipher.decrypt(payload_nonce, encrypted_data)
            .map_err(|e| ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: format!("Decryption failed: {}", e)
                }
            })?;

        debug!("SERVER: Successfully decrypted plaintext ({} bytes): {}",
               plaintext.len(), hex::encode(&plaintext));

        let elapsed = start.elapsed();

        // Замер времени расшифрования
        #[cfg(feature = "metrics")]
        metrics::histogram!("phantom.packet.decryption_time", elapsed.as_micros() as f64);

        debug!(
            "Phantom packet decrypted in {:?}: {} bytes plaintext",
            elapsed,
            plaintext.len()
        );

        Ok(plaintext)
    }

    /// Проверяет подпись пакета
    fn verify_signature(
        &self,
        session: &PhantomSession,
    ) -> ProtocolResult<()> {
        if self.ciphertext.len() < NONCE_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: "Ciphertext too short for nonce".to_string()
            });
        }

        let nonce = &self.ciphertext[..NONCE_SIZE];
        let encrypted_data = &self.ciphertext[NONCE_SIZE..];

        let sign_sequence = self.sequence + 1;

        let mut data_to_verify = Vec::new();
        data_to_verify.extend_from_slice(&self.session_id);
        data_to_verify.extend_from_slice(&sign_sequence.to_be_bytes());
        data_to_verify.push(self.packet_type);

        let nonce_array: [u8; NONCE_SIZE] = nonce.try_into()
            .map_err(|_| ProtocolError::MalformedPacket {
                details: "Invalid nonce length".to_string()
            })?;
        data_to_verify.extend_from_slice(&nonce_array);
        data_to_verify.extend_from_slice(encrypted_data);

        debug!(
            "SERVER: Verifying signature with data:\n\
             - session_id: {}\n\
             - sign_sequence: {} (bytes: {})\n\
             - packet_type: 0x{:02X}\n\
             - nonce: {}\n\
             - encrypted_data: {} bytes\n\
             - packet_sequence: {}\n\
             - total_data_len: {} bytes",
            hex::encode(self.session_id),
            sign_sequence,
            hex::encode(&sign_sequence.to_be_bytes()),
            self.packet_type,
            hex::encode(nonce),
            encrypted_data.len(),
            self.sequence,
            data_to_verify.len()
        );

        let verify_key = session.generate_operation_key_for_sequence(sign_sequence, "auth");

        let mut mac: HmacSha256 = Mac::new_from_slice(verify_key.as_bytes())
            .map_err(|_| ProtocolError::Crypto {
                source: CryptoError::InvalidKeyLength {
                    expected: 32,
                    actual: verify_key.as_bytes().len()
                }
            })?;

        mac.update(&data_to_verify);
        let expected_signature_bytes = mac.finalize().into_bytes();
        let expected_signature: &[u8] = expected_signature_bytes.as_slice();

        debug!("SERVER: Expected signature: {}, received: {}",
               hex::encode(expected_signature),
               hex::encode(&self.signature));

        if !constant_time_eq(expected_signature, &self.signature) {
            warn!(
                "Signature mismatch for session {} packet_sequence={} sign_sequence={}",
                hex::encode(self.session_id),
                self.sequence,
                sign_sequence
            );
            warn!("SERVER: Data for verification hex dump:\n{}",
                  hex::encode(&data_to_verify));
            return Err(ProtocolError::AuthenticationFailed {
                reason: "Invalid signature".to_string()
            });
        }

        debug!(
            "Signature verified for session {} packet_sequence={} sign_sequence={}",
            hex::encode(self.session_id),
            self.sequence,
            sign_sequence
        );

        Ok(())
    }
}

/// Обработчик пакетов с фантомными ключами
pub struct PhantomPacketProcessor;

impl PhantomPacketProcessor {
    pub fn new() -> Self {
        Self
    }

    pub fn process_incoming(
        &self,
        data: &[u8],
        session: &PhantomSession,
    ) -> ProtocolResult<(u8, Vec<u8>)> {
        let packet_start = Instant::now();

        info!("Processing incoming phantom packet: {} bytes", data.len());

        let packet = PhantomPacket::decode(data)?;
        let plaintext = packet.decrypt(session)?;

        if plaintext.len() > MAX_PAYLOAD_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Payload too large: {} bytes", plaintext.len())
            });
        }

        let total_time = packet_start.elapsed();
        info!(
            "Phantom packet processed in {:?}: session={}, type=0x{:02X}, size={} bytes",
            total_time,
            hex::encode(packet.session_id),
            packet.packet_type,
            plaintext.len()
        );

        Ok((packet.packet_type, plaintext))
    }

    pub fn create_outgoing(
        &self,
        session: &PhantomSession,
        packet_type: u8,
        plaintext: &[u8],
    ) -> ProtocolResult<Vec<u8>> {
        let packet = PhantomPacket::create(session, packet_type, plaintext)?;
        Ok(packet.encode())
    }
}

impl Default for PhantomPacketProcessor {
    fn default() -> Self {
        Self::new()
    }
}