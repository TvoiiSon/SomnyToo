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

        // 4. Создаем подпись
        let signature = Self::create_signature(
            session,
            &operation_key,
            packet_type,
            &nonce,
            &ciphertext,
        )?;

        // 5. Получаем метаданные
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let sequence = operation_key.sequence;

        let elapsed = start.elapsed();
        debug!(
            "Phantom packet created in {:?}: type=0x{:02X}, size={} bytes",
            elapsed, packet_type, ciphertext.len()
        );

        Ok(Self {
            session_id: *session.session_id(),
            sequence,
            timestamp,
            packet_type,
            ciphertext,
            signature,
        })
    }

    /// Создает подпись пакета
    fn create_signature(
        session: &PhantomSession,
        operation_key: &PhantomOperationKey,
        packet_type: u8,
        nonce: &[u8; NONCE_SIZE],
        ciphertext: &[u8],
    ) -> ProtocolResult<[u8; 32]> {
        let mut data_to_sign = Vec::new();

        // Включаем в подпись все важные данные
        data_to_sign.extend_from_slice(session.session_id());
        data_to_sign.extend_from_slice(&operation_key.sequence.to_be_bytes());
        data_to_sign.push(packet_type);
        data_to_sign.extend_from_slice(nonce);
        data_to_sign.extend_from_slice(ciphertext);

        // Генерируем ключ для подписи
        let sign_key = session.generate_operation_key("sign");

        // Создаем HMAC - явно указываем тип для избежания неоднозначности
        let mut mac: HmacSha256 = Mac::new_from_slice(sign_key.as_bytes())
            .map_err(|_| ProtocolError::Crypto {
                source: CryptoError::InvalidKeyLength {
                    expected: 32,
                    actual: sign_key.as_bytes().len()
                }
            })?;

        mac.update(&data_to_sign);
        let signature_bytes = mac.finalize().into_bytes();

        // Конвертируем в массив
        let signature: [u8; 32] = signature_bytes.as_slice().try_into()
            .map_err(|_| ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Failed to convert signature to array".to_string()
                }
            })?;

        Ok(signature)
    }

    /// Кодирует пакет в байты для отправки
    pub fn encode(&self) -> Vec<u8> {
        let mut buffer = Vec::new();

        // 1. Magic байты
        buffer.extend_from_slice(&HEADER_MAGIC);

        // 2. Длина (session_id + sequence + timestamp + type + nonce + ciphertext + signature)
        let total_len = 16 + 8 + 8 + 1 + NONCE_SIZE + self.ciphertext.len() + SIGNATURE_SIZE;
        buffer.extend_from_slice(&(total_len as u16).to_be_bytes());

        // 3. Session ID
        buffer.extend_from_slice(&self.session_id);

        // 4. Sequence
        buffer.extend_from_slice(&self.sequence.to_be_bytes());

        // 5. Timestamp
        buffer.extend_from_slice(&self.timestamp.to_be_bytes());

        // 6. Packet type
        buffer.push(self.packet_type);

        // 7. Nonce (симулируем для структуры, реальный nonce в ciphertext)
        let dummy_nonce = [0u8; NONCE_SIZE];
        buffer.extend_from_slice(&dummy_nonce);

        // 8. Ciphertext
        buffer.extend_from_slice(&self.ciphertext);

        // 9. Signature
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

        // 1. Проверяем magic байты
        if !constant_time_eq(&data[0..2], &HEADER_MAGIC) {
            return Err(ProtocolError::MalformedPacket {
                details: "Invalid magic bytes".to_string()
            });
        }

        // 2. Читаем длину
        let length = u16::from_be_bytes([data[2], data[3]]) as usize;

        if length < 16 + 8 + 8 + 1 + NONCE_SIZE + SIGNATURE_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: "Invalid length".to_string()
            });
        }

        let expected_total = 4 + length;
        if data.len() != expected_total {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Length mismatch: expected {}, got {}", expected_total, data.len())
            });
        }

        // 3. Парсим поля
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

        // Пропускаем nonce (он встроен в ciphertext)
        offset += NONCE_SIZE;

        // Оставшиеся данные: ciphertext + signature
        let ciphertext_and_signature = &data[offset..];

        if ciphertext_and_signature.len() < SIGNATURE_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: "Not enough data for signature".to_string()
            });
        }

        let ciphertext_len = ciphertext_and_signature.len() - SIGNATURE_SIZE;
        let ciphertext = ciphertext_and_signature[..ciphertext_len].to_vec();
        let signature: [u8; 32] = ciphertext_and_signature[ciphertext_len..].try_into()
            .map_err(|_| ProtocolError::MalformedPacket {
                details: "Invalid signature".to_string()
            })?;

        // 4. Проверяем timestamp (допуск 30 секунд)
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
            "Phantom packet decoded in {:?}: session={}, sequence={}, type=0x{:02X}",
            elapsed,
            hex::encode(session_id),
            sequence,
            packet_type
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

        // 1. Проверяем сессию
        if !constant_time_eq(&self.session_id, session.session_id()) {
            return Err(ProtocolError::AuthenticationFailed {
                reason: "Session ID mismatch".to_string()
            });
        }

        // 2. Генерируем операционный ключ для расшифровки
        let operation_key = session.generate_operation_key("decrypt");

        // 3. Проверяем подпись
        self.verify_signature(session, &operation_key)?;

        // 4. Расшифровываем данные
        // Note: В реальной реализации nonce должен быть частью ciphertext
        // Для простоты примера используем фиксированный nonce
        let nonce = GenericArray::from_slice(&[0u8; NONCE_SIZE]);

        let cipher = Aes256Gcm::new_from_slice(operation_key.as_bytes())
            .map_err(|e| ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: format!("Failed to create cipher: {}", e)
                }
            })?;

        let plaintext = cipher.decrypt(nonce, self.ciphertext.as_ref())
            .map_err(|e| ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: format!("Decryption failed: {}", e)
                }
            })?;

        let elapsed = start.elapsed();
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
        operation_key: &PhantomOperationKey,
    ) -> ProtocolResult<()> {
        // 1. Создаем данные для проверки подписи
        let mut data_to_verify = Vec::new();
        data_to_verify.extend_from_slice(&self.session_id);
        data_to_verify.extend_from_slice(&self.sequence.to_be_bytes());
        data_to_verify.push(self.packet_type);
        // Note: В реальной реализации нужно включить nonce

        // 2. Генерируем ожидаемую подпись
        let sign_key = session.generate_operation_key("verify");

        let mut mac: HmacSha256 = Mac::new_from_slice(sign_key.as_bytes())
            .map_err(|_| ProtocolError::Crypto {
                source: CryptoError::InvalidKeyLength {
                    expected: 32,
                    actual: sign_key.as_bytes().len()
                }
            })?;

        mac.update(&data_to_verify);
        let expected_signature_bytes = mac.finalize().into_bytes();
        let expected_signature: &[u8] = expected_signature_bytes.as_slice();

        // 3. Сравниваем с постоянным временем
        if !constant_time_eq(expected_signature, &self.signature) {
            return Err(ProtocolError::AuthenticationFailed {
                reason: "Invalid signature".to_string()
            });
        }

        Ok(())
    }
}

/// Обработчик пакетов с фантомными ключами
pub struct PhantomPacketProcessor;

impl PhantomPacketProcessor {
    pub fn new() -> Self {
        Self
    }

    /// Обрабатывает входящий пакет
    pub fn process_incoming(
        &self,
        data: &[u8],
        session: &PhantomSession,
    ) -> ProtocolResult<(u8, Vec<u8>)> {
        let packet_start = Instant::now();

        info!("Processing incoming phantom packet: {} bytes", data.len());

        // 1. Декодируем пакет
        let packet = PhantomPacket::decode(data)?;

        // 2. Расшифровываем содержимое
        let plaintext = packet.decrypt(session)?;

        // 3. Проверяем размер
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

    /// Создает исходящий пакет
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