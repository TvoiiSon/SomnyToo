use std::time::Instant;
use rand_core::{OsRng, RngCore};
use constant_time_eq::constant_time_eq;
use tracing::debug;

use crate::core::protocol::error::{ProtocolResult, ProtocolError, CryptoError};
use crate::core::protocol::phantom_crypto::{
    core::keys::{PhantomSession, PhantomOperationKey},
    acceleration::{
        chacha20_accel::ChaCha20Accelerator,
        blake3_accel::Blake3Accelerator,
    },
};

/// Константы пакетов
pub const HEADER_MAGIC: [u8; 2] = [0xAB, 0xCE];
const NONCE_SIZE: usize = 12;
const TAG_SIZE: usize = 16;
const SIGNATURE_SIZE: usize = 32;
pub const MAX_PAYLOAD_SIZE: usize = 65536; // 64 KB для производительности

/// Пакет с фантомной криптографией (полностью stack allocated)
pub struct PhantomPacket<'a> {
    pub session_id: &'a [u8; 16],
    pub sequence: u64,
    pub timestamp: u64,
    pub packet_type: u8,
    pub ciphertext: &'a [u8],
    pub signature: &'a [u8; 32],
}

impl<'a> PhantomPacket<'a> {
    /// Создает пакет без аллокаций
    pub fn create(
        session: &PhantomSession,
        packet_type: u8,
        plaintext: &[u8],
        buffer: &mut [u8],
        chacha20_accel: &ChaCha20Accelerator,
        blake3_accel: &Blake3Accelerator,
    ) -> ProtocolResult<usize> {
        let start = Instant::now();

        // Проверка размера plaintext
        if plaintext.len() > MAX_PAYLOAD_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Payload too large: {} > {}", plaintext.len(), MAX_PAYLOAD_SIZE)
            });
        }

        // 1. Генерируем операционный ключ
        let operation_key = session.generate_operation_key("encrypt");
        let key_bytes = operation_key.as_bytes();

        // 2. Проверяем размер ключа
        if key_bytes.len() != 32 {
            return Err(ProtocolError::Crypto {
                source: CryptoError::InvalidKeyLength {
                    expected: 32,
                    actual: key_bytes.len(),
                }
            });
        }

        // 3. Генерируем nonce
        let mut nonce = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce);

        debug!("Generated nonce: {} ({} bytes)", hex::encode(&nonce), nonce.len());

        // 4. Рассчитываем размеры
        let header_size = 37; // ИСПРАВЛЕНО: 2 + 2 + 16 + 8 + 8 + 1 = 37 байт!
        let ciphertext_with_tag_len = plaintext.len() + TAG_SIZE;
        let total_size = header_size + NONCE_SIZE + ciphertext_with_tag_len + SIGNATURE_SIZE;

        // Проверка буфера
        if buffer.len() < total_size {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Buffer too small: {} < {}", buffer.len(), total_size)
            });
        }

        // ВАЖНО: Разделяем буфер ПРАВИЛЬНО!
        // header: [0..37]
        // nonce: [37..49]  <- ИСПРАВЛЕНО!
        // ciphertext+tag: [49..49+ciphertext_with_tag_len]
        // signature: [49+ciphertext_with_tag_len..]

        let (header_slice, rest) = buffer.split_at_mut(header_size); // 0..37, 37..
        let (nonce_slice, rest) = rest.split_at_mut(NONCE_SIZE);     // 37..49, 49..
        let (ciphertext_slice, signature_slice) = rest.split_at_mut(ciphertext_with_tag_len); // 49.., остальное

        debug!("Buffer split:");
        debug!("  header slice: [0..{}]", header_size);
        debug!("  nonce slice: [{}..{}]", header_size, header_size + NONCE_SIZE);
        debug!("  ciphertext slice: [{}..{}]", header_size + NONCE_SIZE, header_size + NONCE_SIZE + ciphertext_with_tag_len);
        debug!("  signature slice: [{}..]", header_size + NONCE_SIZE + ciphertext_with_tag_len);

        // 5. Копируем nonce
        debug!("Copying nonce to buffer slice (len: {}): {}", nonce_slice.len(), hex::encode(&nonce));
        nonce_slice.copy_from_slice(&nonce);

        // 6. Копируем plaintext и шифруем
        ciphertext_slice[..plaintext.len()].copy_from_slice(plaintext);

        // Шифрование
        let mut chacha_key = [0u8; 32];
        chacha_key.copy_from_slice(&key_bytes[..32]);

        chacha20_accel.encrypt_in_place(
            &chacha_key,
            &nonce,
            0,
            &mut ciphertext_slice[..plaintext.len()],
        );

        // 7. Добавляем TAG
        // TAG вычисляется на ЗАШИФРОВАННЫХ данных
        let tag = blake3_accel.hash_keyed(&chacha_key, &ciphertext_slice[..plaintext.len()]);
        ciphertext_slice[plaintext.len()..].copy_from_slice(&tag[..TAG_SIZE]);

        debug!("TAG generated (using ENCRYPTED data):");
        debug!("  Key for TAG: {}", hex::encode(&chacha_key));
        debug!("  Encrypted data for TAG: {}", hex::encode(&ciphertext_slice[..plaintext.len()]));
        debug!("  Generated TAG: {}", hex::encode(&tag[..TAG_SIZE]));

        // 8. Создаем подпись
        Self::create_signature_accel(
            session,
            &operation_key,
            packet_type,
            &nonce,
            &ciphertext_slice, // ТОЛЬКО ciphertext_slice (encrypted_data + tag)
            signature_slice,
            blake3_accel,
        )?;

        // 9. Формируем заголовок
        Self::encode_header(
            session,
            operation_key.sequence,
            packet_type,
            (total_size - 4) as u16, // минус magic(2)+length(2)
            header_slice,
        );

        debug!(
            "Packet created in {:?}: total={} bytes, header={}, nonce={}, ciphertext+tag={}, signature={}, seq={}",
            start.elapsed(),
            total_size,
            header_size,
            NONCE_SIZE,
            ciphertext_slice.len(),
            SIGNATURE_SIZE,
            operation_key.sequence
        );

        // Отладочный вывод всего пакета
        debug!("Full packet (first 64 bytes): {}", hex::encode(&buffer[..total_size.min(64)]));

        Ok(total_size)
    }

    #[inline(always)]
    fn create_signature_accel(
        session: &PhantomSession,
        sign_key: &PhantomOperationKey,
        packet_type: u8,
        nonce: &[u8; NONCE_SIZE],
        encrypted_data: &[u8],
        sig_buffer: &mut [u8],
        blake3_accel: &Blake3Accelerator,
    ) -> ProtocolResult<()> {
        // КРИТИЧЕСКИ ВАЖНО: Эти данные должны быть одинаковыми на клиенте и сервере
        let mut input = Vec::with_capacity(16 + 8 + 1 + NONCE_SIZE + encrypted_data.len());

        // 1. session_id
        input.extend_from_slice(session.session_id());

        // 2. sequence number
        input.extend_from_slice(&sign_key.sequence.to_be_bytes());

        // 3. packet_type
        input.push(packet_type);

        // 4. nonce
        input.extend_from_slice(nonce);

        // 5. encrypted_data (ciphertext + tag)
        input.extend_from_slice(encrypted_data);

        // Debug: залогируем данные для подписи
        debug!("Creating signature with:");
        debug!("  session_id: {}", hex::encode(session.session_id()));
        debug!("  sequence: {}", sign_key.sequence);
        debug!("  packet_type: 0x{:02X}", packet_type);
        debug!("  nonce: {}", hex::encode(nonce));
        debug!("  encrypted_data len: {}", encrypted_data.len());
        debug!("  sign_key: {}", hex::encode(sign_key.as_bytes()));

        let signature = blake3_accel.hash_keyed(sign_key.as_bytes(), &input);
        sig_buffer.copy_from_slice(&signature);

        debug!("Created signature: {}", hex::encode(&signature));

        Ok(())
    }

    #[inline(always)]
    fn encode_header(
        session: &PhantomSession,
        sequence: u64,
        packet_type: u8,
        total_len: u16,
        buffer: &mut [u8],
    ) {
        // Записываем MAGIC
        buffer[0..2].copy_from_slice(&HEADER_MAGIC);

        // Записываем длину (без MAGIC и длины - то есть total_len)
        buffer[2..4].copy_from_slice(&total_len.to_be_bytes());

        // Session ID (4..20)
        buffer[4..20].copy_from_slice(session.session_id());

        // Sequence (20..28)
        buffer[20..28].copy_from_slice(&sequence.to_be_bytes());

        // Timestamp (28..36)
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        buffer[28..36].copy_from_slice(&timestamp.to_be_bytes());

        // Packet type (36)
        buffer[36] = packet_type;

        // DEBUG: выведем заголовок
        debug!("Encoded header (37 bytes):");
        debug!("  magic: {}", hex::encode(&buffer[0..2]));
        debug!("  length: {} (0x{:04x})", total_len, total_len);
        debug!("  session_id: {}", hex::encode(&buffer[4..20]));
        debug!("  sequence: {} (0x{:016x})", sequence, sequence);
        debug!("  timestamp: {} (0x{:016x})", timestamp, timestamp);
        debug!("  packet_type: 0x{:02x} at byte 36", packet_type);
        debug!("  full header hex (37 bytes): {}", hex::encode(&buffer[..37]));

        // Проверяем, что после заголовка нет лишних данных
        if buffer.len() > 37 {
            debug!("  bytes 37-38 (should be start of nonce): 0x{:02x}{:02x}",
               buffer[37], buffer[38]);
        }
    }

    /// Декодирует пакет (zero allocation)
    #[inline(always)]
    pub fn decode(data: &'a [u8]) -> ProtocolResult<Self> {
        debug!("Decoding packet of {} bytes", data.len());

        // Минимальная длина: header(37) + nonce(12) + tag(16) + signature(32) = 97 байт
        if data.len() < 97 {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Packet too short: {} < 97", data.len())
            });
        }

        if !constant_time_eq(&data[0..2], &HEADER_MAGIC) {
            return Err(ProtocolError::MalformedPacket {
                details: "Invalid magic bytes".to_string()
            });
        }

        let length = u16::from_be_bytes([data[2], data[3]]) as usize;
        debug!("Length from header: {} (packet should be {} bytes)", length, 4 + length);

        // Проверяем длину пакета
        if data.len() != 4 + length {
            debug!("Warning: actual packet length {} != 4 + {}", data.len(), length);
            // Но продолжаем с фактической длиной
        }

        // Заголовок: 37 байт
        let session_id: &[u8; 16] = data[4..20].try_into().unwrap();
        let sequence = u64::from_be_bytes(data[20..28].try_into().unwrap());
        let _timestamp = u64::from_be_bytes(data[28..36].try_into().unwrap());
        let packet_type = data[36];

        debug!("Parsed header (bytes 0-36):");
        debug!("  session_id: {}", hex::encode(session_id));
        debug!("  sequence: {}", sequence);
        debug!("  packet_type: 0x{:02x} at byte 36", packet_type);

        // Nonce начинается с 37 байта (после packet_type)
        let nonce_start = 37;
        let nonce_end = nonce_start + NONCE_SIZE;

        if nonce_end > data.len() {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Packet too short for nonce: {} < {}", data.len(), nonce_end)
            });
        }

        // Проверяем nonce
        let nonce_bytes = &data[nonce_start..nonce_end];
        debug!("Nonce (bytes 37-48): {}", hex::encode(nonce_bytes));

        // Ciphertext: nonce + encrypted_data_with_tag
        let ciphertext_end = data.len() - SIGNATURE_SIZE;

        if ciphertext_end <= nonce_end {
            return Err(ProtocolError::MalformedPacket {
                details: "Packet too short for ciphertext".to_string()
            });
        }

        let ciphertext = &data[nonce_start..ciphertext_end];

        if ciphertext_end + SIGNATURE_SIZE > data.len() {
            return Err(ProtocolError::MalformedPacket {
                details: "No room for signature".to_string()
            });
        }

        let signature: &[u8; 32] = data[ciphertext_end..ciphertext_end + 32].try_into().unwrap();

        debug!("Decoded successfully:");
        debug!("  ciphertext total: {} bytes (nonce + encrypted_data+tag)", ciphertext.len());
        debug!("  signature: {} bytes", signature.len());

        Ok(Self {
            session_id,
            sequence,
            timestamp: 0,
            packet_type,
            ciphertext,
            signature,
        })
    }

    /// Расшифровывает пакет (zero allocation)
    #[inline(always)]
    pub fn decrypt(
        &self,
        session: &PhantomSession,
        work_buffer: &mut [u8],     // Временный буфер (plaintext_len + TAG_SIZE)
        output: &mut [u8],          // Выходной буфер
        chacha20_accel: &ChaCha20Accelerator,
        blake3_accel: &Blake3Accelerator,
    ) -> ProtocolResult<(u8, usize)> {
        // 1. Проверка session_id
        if !constant_time_eq(self.session_id, session.session_id()) {
            return Err(ProtocolError::AuthenticationFailed {
                reason: "Session ID mismatch".to_string()
            });
        }

        // 2. Проверка signature
        self.verify_signature_accel(session, blake3_accel)?;

        // 3. Извлекаем nonce и данные
        if self.ciphertext.len() < NONCE_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Ciphertext too short for nonce: {} < {}",
                                 self.ciphertext.len(), NONCE_SIZE)
            });
        }

        let nonce = &self.ciphertext[..NONCE_SIZE];
        let encrypted_data = &self.ciphertext[NONCE_SIZE..];

        if encrypted_data.len() < TAG_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Data too short for TAG: {} < {}",
                                 encrypted_data.len(), TAG_SIZE)
            });
        }

        let data_len = encrypted_data.len() - TAG_SIZE;

        debug!("Decrypting: nonce={}, encrypted_data_len={}, data_len={}, TAG_SIZE={}",
           hex::encode(nonce), encrypted_data.len(), data_len, TAG_SIZE);

        // 4. Генерируем ключ дешифрования
        let decrypt_key = session.generate_operation_key_for_sequence(self.sequence, "encrypt");
        let key_bytes = decrypt_key.as_bytes();

        debug!("Decryption key: {}", hex::encode(key_bytes));
        debug!("Key sequence: {}", self.sequence);

        // 5. Дешифруем с аппаратным ускорением
        // Копируем зашифрованные данные (без TAG) в work_buffer
        if work_buffer.len() < data_len {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Work buffer too small: {} < {}", work_buffer.len(), data_len)
            });
        }

        work_buffer[..data_len].copy_from_slice(&encrypted_data[..data_len]);

        let mut chacha_key = [0u8; 32];
        chacha_key.copy_from_slice(&key_bytes[..32]);

        debug!("Decrypting {} bytes with key: {}", data_len, hex::encode(&chacha_key));

        // Сохраняем оригинальные зашифрованные данные для проверки TAG
        let mut encrypted_copy = vec![0u8; data_len];
        encrypted_copy.copy_from_slice(&encrypted_data[..data_len]);

        chacha20_accel.encrypt_in_place(
            &chacha_key,
            nonce.try_into().unwrap(),
            0,
            &mut work_buffer[..data_len],
        );

        // 6. Проверяем TAG
        // TAG должен вычисляться на ЗАШИФРОВАННЫХ данных (как делает клиент)
        let received_tag = &encrypted_data[data_len..data_len + TAG_SIZE];

        // Вычисляем ожидаемый TAG на ЗАШИФРОВАННЫХ данных
        let expected_tag = blake3_accel.hash_keyed(&chacha_key, &encrypted_copy);

        debug!("TAG check (using ENCRYPTED data):");
        debug!("  Encrypted data: {}", hex::encode(&encrypted_copy));
        debug!("  Received TAG: {}", hex::encode(received_tag));
        debug!("  Expected TAG: {}", hex::encode(&expected_tag[..TAG_SIZE]));

        if !constant_time_eq(&expected_tag[..TAG_SIZE], received_tag) {
            return Err(ProtocolError::AuthenticationFailed {
                reason: format!("Invalid TAG. Key sequence: {}, nonce: {}",
                                self.sequence, hex::encode(nonce))
            });
        }

        // 7. Копируем результат
        let output_len = data_len.min(output.len());
        output[..output_len].copy_from_slice(&work_buffer[..output_len]);

        debug!("Successfully decrypted {} bytes", output_len);

        Ok((self.packet_type, output_len))
    }

    #[inline(always)]
    fn verify_signature_accel(
        &self,
        session: &PhantomSession,
        blake3_accel: &Blake3Accelerator,
    ) -> ProtocolResult<()> {
        // Проверяем, что ciphertext содержит хотя бы nonce
        if self.ciphertext.len() < NONCE_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Ciphertext too short for nonce: {} < {}",
                                 self.ciphertext.len(), NONCE_SIZE)
            });
        }

        let nonce = &self.ciphertext[..NONCE_SIZE];
        let encrypted_data = &self.ciphertext[NONCE_SIZE..];

        // Debug: выведем фактически полученные данные
        debug!("In verify_signature_accel:");
        debug!("  ciphertext total len: {}", self.ciphertext.len());
        debug!("  nonce len: {}", nonce.len());
        debug!("  nonce hex: {}", hex::encode(nonce));
        debug!("  encrypted_data len: {}", encrypted_data.len());
        debug!("  encrypted_data first 16 bytes: {}", hex::encode(&encrypted_data[..encrypted_data.len().min(16)]));

        // Генерируем ключ верификации
        let verify_key = session.generate_operation_key_for_sequence(self.sequence, "encrypt");
        let key_bytes = verify_key.as_bytes();

        // Вычисляем ожидаемую подпись
        let mut input = Vec::with_capacity(16 + 8 + 1 + NONCE_SIZE + encrypted_data.len());
        input.extend_from_slice(self.session_id);
        input.extend_from_slice(&self.sequence.to_be_bytes());
        input.push(self.packet_type);
        input.extend_from_slice(nonce);
        input.extend_from_slice(encrypted_data);

        // Debug: покажем, что именно подписываем
        debug!("Signing input:");
        debug!("  session_id: {} ({} bytes)", hex::encode(self.session_id), self.session_id.len());
        debug!("  sequence: {} ({} bytes)", self.sequence, 8);
        debug!("  packet_type: 0x{:02X} (1 byte)", self.packet_type);
        debug!("  nonce: {} ({} bytes)", hex::encode(nonce), nonce.len());
        debug!("  encrypted_data: {} ({} bytes)",
           hex::encode(&encrypted_data[..encrypted_data.len().min(16)]),
           encrypted_data.len());
        debug!("  total input size: {} bytes", input.len());
        debug!("  key: {}", hex::encode(key_bytes));

        let expected_signature = blake3_accel.hash_keyed(key_bytes, &input);

        debug!("Expected signature: {}", hex::encode(&expected_signature));
        debug!("Actual signature: {}", hex::encode(self.signature));

        if !constant_time_eq(&expected_signature, self.signature) {
            return Err(ProtocolError::AuthenticationFailed {
                reason: format!("Invalid signature. Input for signature: session_id={}, sequence={}, packet_type=0x{:02X}, nonce={}, encrypted_data_len={}",
                                hex::encode(self.session_id),
                                self.sequence,
                                self.packet_type,
                                hex::encode(nonce),
                                encrypted_data.len())
            });
        }

        Ok(())
    }
}

/// Высокопроизводительный процессор пакетов
pub struct PhantomPacketProcessor {
    chacha20_accel: ChaCha20Accelerator,
    blake3_accel: Blake3Accelerator,
}

impl PhantomPacketProcessor {
    pub fn new() -> Self {
        Self {
            chacha20_accel: ChaCha20Accelerator::new(),
            blake3_accel: Blake3Accelerator::new(),
        }
    }

    #[inline]
    pub fn process_incoming_vec(
        &self,
        data: &[u8],
        session: &PhantomSession,
    ) -> ProtocolResult<(u8, Vec<u8>)> {
        let mut work_buffer = vec![0u8; MAX_PAYLOAD_SIZE + TAG_SIZE];
        let mut output_buffer = vec![0u8; MAX_PAYLOAD_SIZE];

        let packet = PhantomPacket::decode(data)?;

        let (packet_type, size) = packet.decrypt(
            session,
            &mut work_buffer,
            &mut output_buffer,
            &self.chacha20_accel,
            &self.blake3_accel,
        )?;

        // Возвращаем вектор с данными
        output_buffer.truncate(size);
        Ok((packet_type, output_buffer))
    }

    #[inline]
    pub fn create_outgoing_vec(
        &self,
        session: &PhantomSession,
        packet_type: u8,
        plaintext: &[u8],
    ) -> ProtocolResult<Vec<u8>> {
        // Проверка размера plaintext
        if plaintext.len() > MAX_PAYLOAD_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Payload too large: {} > {}", plaintext.len(), MAX_PAYLOAD_SIZE)
            });
        }

        // Рассчитываем реальный размер пакета
        let header_size = 37; // ИСПРАВЛЕНО!
        let total_size = header_size + NONCE_SIZE + plaintext.len() + TAG_SIZE + SIGNATURE_SIZE;

        // Создаем буфер точного размера
        let mut buffer = vec![0u8; total_size];

        let size = PhantomPacket::create(
            session,
            packet_type,
            plaintext,
            &mut buffer,
            &self.chacha20_accel,
            &self.blake3_accel,
        )?;

        // Проверяем размер
        if size != total_size {
            debug!("Warning: created size {} != calculated size {}", size, total_size);
        }

        buffer.truncate(size);

        // Проверяем структуру
        debug!("Created packet structure check:");
        debug!("  total size: {}", buffer.len());
        debug!("  magic: {}", hex::encode(&buffer[0..2]));
        debug!("  length field: {}", u16::from_be_bytes([buffer[2], buffer[3]]));
        debug!("  packet_type at byte 36: 0x{:02x}", buffer[36]);
        debug!("  nonce starts at byte 37: {}", hex::encode(&buffer[37..49]));

        Ok(buffer)
    }

    #[inline]
    pub fn process_incoming_slice(
        &self,
        data: &[u8],
        session: &PhantomSession,
        work_buffer: &mut [u8],
        output_buffer: &mut [u8],
    ) -> ProtocolResult<(u8, usize)> {
        let packet = PhantomPacket::decode(data)?;

        packet.decrypt(
            session,
            work_buffer,
            output_buffer,
            &self.chacha20_accel,
            &self.blake3_accel,
        )
    }

    #[inline]
    pub fn create_outgoing_slice(
        &self,
        session: &PhantomSession,
        packet_type: u8,
        plaintext: &[u8],
        buffer: &mut [u8],
    ) -> ProtocolResult<usize> {
        PhantomPacket::create(
            session,
            packet_type,
            plaintext,
            buffer,
            &self.chacha20_accel,
            &self.blake3_accel,
        )
    }

    // Для обратной совместимости
    #[inline]
    pub fn process_incoming(
        &self,
        data: &[u8],
        session: &PhantomSession,
    ) -> ProtocolResult<(u8, Vec<u8>)> {
        self.process_incoming_vec(data, session)
    }

    #[inline]
    pub fn create_outgoing(
        &self,
        session: &PhantomSession,
        packet_type: u8,
        plaintext: &[u8],
    ) -> ProtocolResult<Vec<u8>> {
        self.create_outgoing_vec(session, packet_type, plaintext)
    }
}

impl Clone for PhantomPacketProcessor {
    fn clone(&self) -> Self {
        Self {
            chacha20_accel: self.chacha20_accel.clone(),
            blake3_accel: self.blake3_accel.clone(),
        }
    }
}

impl Default for PhantomPacketProcessor {
    fn default() -> Self {
        Self::new()
    }
}