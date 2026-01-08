use std::time::Instant;
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, KeyInit, AeadInPlace};
use blake3::Hasher;
use rand_core::{OsRng, RngCore};
use constant_time_eq::constant_time_eq;
use tracing::{info, warn, debug};

use super::keys::{PhantomSession, PhantomOperationKey};
use crate::core::protocol::error::{ProtocolResult, ProtocolError, CryptoError};

/// Константы пакетов
pub const HEADER_MAGIC: [u8; 2] = [0xAB, 0xCE];
const NONCE_SIZE: usize = 12;
const TAG_SIZE: usize = 16;
const SIGNATURE_SIZE: usize = 32;
const MAX_PAYLOAD_SIZE: usize = 1 << 20; // 1 MB

/// Пакет с фантомной криптографией (без аллокаций внутри)
pub struct PhantomPacket<'a> {
    pub session_id: &'a [u8; 16],
    pub sequence: u64,
    pub timestamp: u64,
    pub packet_type: u8,
    pub ciphertext: &'a [u8], // Ссылка на данные, а не владение
    pub signature: &'a [u8; 32],
}

impl<'a> PhantomPacket<'a> {
    /// Создает новый пакет для отправки (без аллокаций в hot path)
    pub fn create<'b>(
        session: &PhantomSession,
        packet_type: u8,
        plaintext: &[u8],
        buffer: &'b mut [u8], // Предвыделенный буфер
    ) -> ProtocolResult<&'b [u8]> {
        let start = Instant::now();

        info!("Creating phantom packet: type=0x{:02X}, size={} bytes", packet_type, plaintext.len());

        // 1. Генерируем операционный ключ для шифрования
        let operation_key = session.generate_operation_key("encrypt");
        let key_bytes = operation_key.as_bytes();
        let sequence = operation_key.sequence;

        // 2. Генерируем nonce
        let mut nonce = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce);

        debug!("Generated nonce for encryption: {}", hex::encode(&nonce));

        // 3. Шифруем данные с ChaCha20Poly1305
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key_bytes));
        let payload_nonce = Nonce::from_slice(&nonce);

        // Рассчитываем размеры
        let header_size = 4 + 16 + 8 + 8 + 1; // magic(2) + len(2) + session_id(16) + sequence(8) + timestamp(8) + type(1) = 39 байт
        let nonce_start = header_size; // nonce начинается после заголовка
        let ciphertext_start = nonce_start + NONCE_SIZE; // ciphertext начинается после nonce

        // Для encrypt_in_place нам нужно место для plaintext + TAG
        let ciphertext_with_tag_len = plaintext.len() + TAG_SIZE;
        let ciphertext_with_tag_end = ciphertext_start + ciphertext_with_tag_len;
        let signature_start = ciphertext_with_tag_end; // подпись начинается после ciphertext+TAG

        // Общий размер пакета: header + nonce + ciphertext + TAG + signature
        let total_size = signature_start + SIGNATURE_SIZE;

        if buffer.len() < total_size {
            return Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: format!("Buffer too small: need {}, have {}", total_size, buffer.len())
                }
            });
        }

        // Для шифрования используем временный вектор
        let mut encrypt_buffer = Vec::with_capacity(plaintext.len() + TAG_SIZE);
        encrypt_buffer.extend_from_slice(plaintext);

        // Шифруем in-place
        match cipher.encrypt_in_place(payload_nonce, &[], &mut encrypt_buffer) {
            Ok(_) => {
                // Теперь encrypt_buffer содержит: [ciphertext...][TAG...] (plaintext.len() + TAG_SIZE байт)

                // Разделяем буфер на части для безопасной записи
                let (header_part, rest) = buffer.split_at_mut(header_size);
                let (nonce_part, ciphertext_and_sig_part) = rest.split_at_mut(NONCE_SIZE);

                // Копируем nonce
                nonce_part.copy_from_slice(&nonce);

                // Копируем ciphertext + tag
                let ciphertext_part = &mut ciphertext_and_sig_part[..encrypt_buffer.len()];
                ciphertext_part.copy_from_slice(&encrypt_buffer);

                // Создаем подпись
                let signature_start_in_part = encrypt_buffer.len();
                let signature_buffer = &mut ciphertext_and_sig_part[signature_start_in_part..signature_start_in_part + SIGNATURE_SIZE];
                let signature = Self::create_signature(
                    session,
                    &operation_key,
                    packet_type,
                    &nonce,
                    &encrypt_buffer,
                    signature_buffer,
                )?;

                // Формируем заголовок в отдельной части буфера
                Self::encode_header(
                    session,
                    sequence,
                    packet_type,
                    (total_size - 4) as u16, // total_len не включает magic(2) + len(2)
                    header_part,
                );

                // Подпись уже записана в create_signature
                let _ = signature; // Используем переменную для подавления warning

                let elapsed = start.elapsed();
                debug!(
                    "Phantom packet created in {:?}: type=0x{:02X}, total_size={} bytes (header={}, nonce={}, ciphertext+tag={}, sig={})",
                    elapsed, packet_type, total_size, header_size, NONCE_SIZE, encrypt_buffer.len(), SIGNATURE_SIZE
                );

                Ok(&buffer[..total_size])
            }
            Err(e) => Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: format!("Encryption failed: {}", e)
                }
            }),
        }
    }

    /// Создает подпись пакета с Blake3 (быстрее HMAC)
    fn create_signature<'b>(
        session: &PhantomSession,
        sign_key: &PhantomOperationKey,
        packet_type: u8,
        nonce: &[u8; NONCE_SIZE],
        encrypted_data: &[u8],
        sig_buffer: &'b mut [u8],
    ) -> ProtocolResult<&'b [u8; 32]> {
        let mut hasher = Hasher::new_keyed(sign_key.as_bytes());

        hasher.update(session.session_id());
        hasher.update(&sign_key.sequence.to_be_bytes());
        hasher.update(&[packet_type]);
        hasher.update(nonce);
        hasher.update(encrypted_data);

        let signature = hasher.finalize();

        if sig_buffer.len() < 32 {
            return Err(ProtocolError::Crypto {
                source: CryptoError::InvalidKeyLength {
                    expected: 32,
                    actual: sig_buffer.len()
                }
            });
        }

        sig_buffer[..32].copy_from_slice(signature.as_bytes());

        // Безопасное преобразование
        Ok(unsafe { &*(sig_buffer.as_ptr() as *const [u8; 32]) })
    }

    /// Кодирует заголовок пакета
    fn encode_header(
        session: &PhantomSession,
        sequence: u64,
        packet_type: u8,
        total_len: u16,
        buffer: &mut [u8],
    ) {
        buffer[0..2].copy_from_slice(&HEADER_MAGIC);
        buffer[2..4].copy_from_slice(&total_len.to_be_bytes());
        buffer[4..20].copy_from_slice(session.session_id());
        buffer[20..28].copy_from_slice(&sequence.to_be_bytes());

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        buffer[28..36].copy_from_slice(&timestamp.to_be_bytes());
        buffer[36] = packet_type;
    }

    /// Декодирует пакет из байтов (без аллокаций)
    pub fn decode(data: &'a [u8]) -> ProtocolResult<Self> {
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
        let min_length = 4 + 16 + 8 + 8 + 1 + NONCE_SIZE + TAG_SIZE + SIGNATURE_SIZE;

        if length < min_length {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Invalid length: {} (min: {})", length, min_length)
            });
        }

        // ВАЖНО: length - это длина данных БЕЗ magic(2) и len(2)
        // total_packet_size = 4 + length
        let total_packet_size = 4 + length;

        if data.len() != total_packet_size {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Length mismatch: expected {} (4 + {}), got {}",
                                 total_packet_size, length, data.len())
            });
        }

        // Парсим заголовок (только смещения, без аллокаций)
        let session_id: &[u8; 16] = data[4..20].try_into()
            .map_err(|_| ProtocolError::MalformedPacket {
                details: "Invalid session id".to_string()
            })?;

        let sequence = u64::from_be_bytes(
            data[20..28].try_into()
                .map_err(|_| ProtocolError::MalformedPacket {
                    details: "Invalid sequence".to_string()
                })?
        );

        let timestamp = u64::from_be_bytes(
            data[28..36].try_into()
                .map_err(|_| ProtocolError::MalformedPacket {
                    details: "Invalid timestamp".to_string()
                })?
        );

        let packet_type = data[36];

        // Ciphertext начинается с nonce (после заголовка)
        let ciphertext_start = 37; // 4(magic+len) + 33(header без magic+len) = 37
        let ciphertext_end = length - SIGNATURE_SIZE + 4; // +4 для magic+len

        if ciphertext_end > data.len() {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Invalid ciphertext range: {}-{} in {} bytes",
                                 ciphertext_start, ciphertext_end, data.len())
            });
        }

        let ciphertext = &data[ciphertext_start..ciphertext_end];

        let signature: &[u8; 32] = data[ciphertext_end..ciphertext_end + 32].try_into()
            .map_err(|_| ProtocolError::MalformedPacket {
                details: "Invalid signature".to_string()
            })?;

        let elapsed = start.elapsed();
        debug!(
            "Phantom packet decoded in {:?}: session={}, sequence={}, type=0x{:02X}, ciphertext={} bytes, total={} bytes",
            elapsed,
            hex::encode(session_id),
            sequence,
            packet_type,
            ciphertext.len(),
            data.len()
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

    /// Расшифровывает содержимое пакета (без аллокаций)
    pub fn decrypt<'b>(
        &self,
        session: &PhantomSession,
        plaintext_buffer: &'b mut [u8],
    ) -> ProtocolResult<&'b [u8]> {
        let start = Instant::now();

        debug!("Decrypting phantom packet: session={}, sequence={}, ciphertext={} bytes",
               hex::encode(self.session_id), self.sequence, self.ciphertext.len());

        if !constant_time_eq(self.session_id, session.session_id()) {
            return Err(ProtocolError::AuthenticationFailed {
                reason: "Session ID mismatch".to_string()
            });
        }

        self.verify_signature(session)?;

        if self.ciphertext.len() < NONCE_SIZE + TAG_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Ciphertext too short: {} bytes, need at least {}",
                                 self.ciphertext.len(), NONCE_SIZE + TAG_SIZE)
            });
        }

        let nonce = &self.ciphertext[..NONCE_SIZE];
        let encrypted_data_with_tag = &self.ciphertext[NONCE_SIZE..];

        if encrypted_data_with_tag.len() < TAG_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Encrypted data too short: {} bytes, need at least {}",
                                 encrypted_data_with_tag.len(), TAG_SIZE)
            });
        }

        // Генерируем ключ дешифрования
        let decrypt_key = session.generate_operation_key_for_sequence(self.sequence, "encrypt");
        let key_bytes = decrypt_key.as_bytes();

        // Создаем cipher
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key_bytes));
        let payload_nonce = Nonce::from_slice(nonce);

        // Для дешифрования нам нужно скопировать данные во временный буфер
        let mut decrypt_buffer = encrypted_data_with_tag.to_vec();

        // Дешифруем in-place
        match cipher.decrypt_in_place(
            payload_nonce,
            &[], // AAD
            &mut decrypt_buffer
        ) {
            Ok(_) => {
                // Результат теперь в decrypt_buffer, но без TAG
                let plaintext_len = decrypt_buffer.len();

                if plaintext_buffer.len() < plaintext_len {
                    return Err(ProtocolError::Crypto {
                        source: CryptoError::DecryptionFailed {
                            reason: format!("Plaintext buffer too small: need {}, have {}",
                                            plaintext_len, plaintext_buffer.len())
                        }
                    });
                }

                plaintext_buffer[..plaintext_len].copy_from_slice(&decrypt_buffer);

                let elapsed = start.elapsed();
                debug!(
                    "Phantom packet decrypted in {:?}: {} bytes plaintext",
                    elapsed,
                    plaintext_len
                );

                Ok(&plaintext_buffer[..plaintext_len])
            }
            Err(e) => Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: format!("Decryption failed: {}", e)
                }
            }),
        }
    }

    /// Проверяет подпись пакета с Blake3
    fn verify_signature(
        &self,
        session: &PhantomSession,
    ) -> ProtocolResult<()> {
        if self.ciphertext.len() < NONCE_SIZE + TAG_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: "Ciphertext too short for verification".to_string()
            });
        }

        let nonce = &self.ciphertext[..NONCE_SIZE];
        let encrypted_data_with_tag = &self.ciphertext[NONCE_SIZE..];

        if encrypted_data_with_tag.len() < TAG_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: "Encrypted data too short".to_string()
            });
        }

        // ИСПРАВЛЕНИЕ: используем ТОТ ЖЕ sequence что и клиент
        // sign_key.sequence на клиенте = self.sequence на сервере
        debug!(
            "Verifying signature for session={}, packet_sequence={}",
            hex::encode(self.session_id),
            self.sequence
        );

        let verify_key = session.generate_operation_key_for_sequence(self.sequence, "encrypt");

        // Blake3 для верификации
        let mut hasher = Hasher::new_keyed(verify_key.as_bytes());
        hasher.update(self.session_id);
        hasher.update(&self.sequence.to_be_bytes());  // ИСПРАВЛЕНИЕ: без +1
        hasher.update(&[self.packet_type]);
        hasher.update(nonce);
        hasher.update(encrypted_data_with_tag);

        let expected_signature = hasher.finalize();

        if !constant_time_eq(expected_signature.as_bytes(), self.signature) {
            warn!(
            "Signature mismatch for session {} sequence={}",
            hex::encode(self.session_id),
            self.sequence
        );
            return Err(ProtocolError::AuthenticationFailed {
                reason: "Invalid signature".to_string()
            });
        }

        debug!(
            "Signature verified for session {} sequence={}",
            hex::encode(self.session_id),
            self.sequence
        );

        Ok(())
    }
}

/// Обработчик пакетов с оптимизированной памятью
pub struct PhantomPacketProcessor {
    // Предвыделенные буферы для обработки
    encrypt_buffer: Vec<u8>,
    decrypt_buffer: Vec<u8>,
    temp_buffer: Vec<u8>, // Временный буфер для дешифрования
}

impl PhantomPacketProcessor {
    pub fn new() -> Self {
        Self {
            encrypt_buffer: vec![0; MAX_PAYLOAD_SIZE + 1024], // + запас
            decrypt_buffer: vec![0; MAX_PAYLOAD_SIZE],
            temp_buffer: vec![0; MAX_PAYLOAD_SIZE + TAG_SIZE],
        }
    }

    pub fn with_capacity(encrypt_cap: usize, decrypt_cap: usize) -> Self {
        Self {
            encrypt_buffer: vec![0; encrypt_cap],
            decrypt_buffer: vec![0; decrypt_cap],
            temp_buffer: vec![0; decrypt_cap + TAG_SIZE],
        }
    }

    pub fn process_incoming(
        &mut self,
        data: &[u8],
        session: &PhantomSession,
    ) -> ProtocolResult<(u8, Vec<u8>)> {
        let packet_start = Instant::now();

        info!("Processing incoming phantom packet: {} bytes", data.len());

        let packet = PhantomPacket::decode(data)?;

        // Используем предвыделенный буфер
        let plaintext = packet.decrypt(session, &mut self.decrypt_buffer)?;

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

        Ok((packet.packet_type, plaintext.to_vec()))
    }

    pub fn create_outgoing<'a>(
        &'a mut self,
        session: &PhantomSession,
        packet_type: u8,
        plaintext: &[u8],
    ) -> ProtocolResult<&'a [u8]> {
        PhantomPacket::create(session, packet_type, plaintext, &mut self.encrypt_buffer)
    }

    /// Создает исходящий пакет и возвращает Vec<u8> для совместимости
    pub fn create_outgoing_vec(
        &mut self,
        session: &PhantomSession,
        packet_type: u8,
        plaintext: &[u8],
    ) -> ProtocolResult<Vec<u8>> {
        self.create_outgoing(session, packet_type, plaintext)
            .map(|slice| slice.to_vec())
    }
}

impl Default for PhantomPacketProcessor {
    fn default() -> Self {
        Self::new()
    }
}