use std::time::Instant;
use rand_core::{OsRng, RngCore};
use constant_time_eq::constant_time_eq;
use tracing::{debug, info};

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
        let total_start = Instant::now();
        let mut stages_time = Vec::new();

        // Проверка размера plaintext
        if plaintext.len() > MAX_PAYLOAD_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Payload too large: {} > {}", plaintext.len(), MAX_PAYLOAD_SIZE)
            });
        }

        // 1. Генерируем операционный ключ
        let key_gen_start = Instant::now();
        let operation_key = session.generate_operation_key("encrypt");
        let key_bytes = operation_key.as_bytes();
        let key_gen_time = key_gen_start.elapsed();
        stages_time.push(("generate_operation_key", key_gen_time));

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
        let nonce_gen_start = Instant::now();
        let mut nonce = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce);
        let nonce_gen_time = nonce_gen_start.elapsed();
        stages_time.push(("generate_nonce", nonce_gen_time));

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
        let buffer_split_start = Instant::now();
        let (header_slice, rest) = buffer.split_at_mut(header_size);
        let (nonce_slice, rest) = rest.split_at_mut(NONCE_SIZE);
        let (ciphertext_slice, signature_slice) = rest.split_at_mut(ciphertext_with_tag_len);
        let buffer_split_time = buffer_split_start.elapsed();
        stages_time.push(("buffer_split", buffer_split_time));

        // 5. Копируем nonce
        let nonce_copy_start = Instant::now();
        nonce_slice.copy_from_slice(&nonce);
        let nonce_copy_time = nonce_copy_start.elapsed();
        stages_time.push(("nonce_copy", nonce_copy_time));

        // 6. Копируем plaintext и шифруем
        let encryption_start = Instant::now();
        ciphertext_slice[..plaintext.len()].copy_from_slice(plaintext);

        let mut chacha_key = [0u8; 32];
        chacha_key.copy_from_slice(&key_bytes[..32]);

        chacha20_accel.encrypt_in_place(
            &chacha_key,
            &nonce,
            0,
            &mut ciphertext_slice[..plaintext.len()],
        );
        let encryption_time = encryption_start.elapsed();
        stages_time.push(("encryption", encryption_time));

        // 7. Добавляем TAG
        let tag_gen_start = Instant::now();
        let tag = blake3_accel.hash_keyed(&chacha_key, &ciphertext_slice[..plaintext.len()]);
        ciphertext_slice[plaintext.len()..].copy_from_slice(&tag[..TAG_SIZE]);
        let tag_gen_time = tag_gen_start.elapsed();
        stages_time.push(("tag_generation", tag_gen_time));

        // 8. Создаем подпись
        let signature_start = Instant::now();
        Self::create_signature_accel(
            session,
            &operation_key,
            packet_type,
            &nonce,
            &ciphertext_slice, // ТОЛЬКО ciphertext_slice (encrypted_data + tag)
            signature_slice,
            blake3_accel,
        )?;
        let signature_time = signature_start.elapsed();
        stages_time.push(("signature_creation", signature_time));

        // 9. Формируем заголовок
        let header_start = Instant::now();
        Self::encode_header(
            session,
            operation_key.sequence,
            packet_type,
            (total_size - 4) as u16, // минус magic(2)+length(2)
            header_slice,
        );
        let header_time = header_start.elapsed();
        stages_time.push(("header_encoding", header_time));

        let total_time = total_start.elapsed();

        // Логируем время выполнения каждого этапа
        info!("PACKET CREATION PERFORMANCE:");
        info!("  Total time: {:?} ({:.2} ms)", total_time, total_time.as_micros() as f64 / 1000.0);

        for (stage_name, duration) in &stages_time {
            info!("  {}: {:?} ({:.2} µs, {:.1}%)",
                  stage_name,
                  duration,
                  duration.as_nanos() as f64 / 1000.0,
                  (duration.as_nanos() as f64 / total_time.as_nanos() as f64) * 100.0);
        }

        debug!(
            "Packet created in {:?}: total={} bytes, header={}, nonce={}, ciphertext+tag={}, signature={}, seq={}",
            total_time,
            total_size,
            header_size,
            NONCE_SIZE,
            ciphertext_slice.len(),
            SIGNATURE_SIZE,
            operation_key.sequence
        );

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
        let sig_start = Instant::now();

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

        let signature = blake3_accel.hash_keyed(sign_key.as_bytes(), &input);
        sig_buffer.copy_from_slice(&signature);

        let sig_time = sig_start.elapsed();
        debug!("Signature creation time: {:?} ({:.2} µs)", sig_time, sig_time.as_nanos() as f64 / 1000.0);

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
        let start = Instant::now();

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

        let elapsed = start.elapsed();
        debug!("Header encoding time: {:?} ({:.2} µs)", elapsed, elapsed.as_nanos() as f64 / 1000.0);
    }

    /// Декодирует пакет (zero allocation)
    #[inline(always)]
    pub fn decode(data: &'a [u8]) -> ProtocolResult<Self> {
        let decode_start = Instant::now();

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

        // Проверяем длину пакета
        if data.len() != 4 + length {
            debug!("Warning: actual packet length {} != 4 + {}", data.len(), length);
        }

        // Заголовок: 37 байт
        let session_id: &[u8; 16] = data[4..20].try_into().unwrap();
        let sequence = u64::from_be_bytes(data[20..28].try_into().unwrap());
        let _timestamp = u64::from_be_bytes(data[28..36].try_into().unwrap());
        let packet_type = data[36];

        // Nonce начинается с 37 байта (после packet_type)
        let nonce_start = 37;
        let nonce_end = nonce_start + NONCE_SIZE;

        if nonce_end > data.len() {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Packet too short for nonce: {} < {}", data.len(), nonce_end)
            });
        }

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

        let decode_time = decode_start.elapsed();
        debug!("Packet decode time: {:?} ({:.2} µs)", decode_time, decode_time.as_nanos() as f64 / 1000.0);

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
        let total_start = Instant::now();
        let mut stages_time = Vec::new();

        // 1. Проверка session_id
        let session_check_start = Instant::now();
        if !constant_time_eq(self.session_id, session.session_id()) {
            return Err(ProtocolError::AuthenticationFailed {
                reason: "Session ID mismatch".to_string()
            });
        }
        let session_check_time = session_check_start.elapsed();
        stages_time.push(("session_id_check", session_check_time));

        // 2. Проверка signature
        let signature_start = Instant::now();
        self.verify_signature_accel(session, blake3_accel)?;
        let signature_time = signature_start.elapsed();
        stages_time.push(("signature_verification", signature_time));

        // 3. Извлекаем nonce и данные
        let extract_start = Instant::now();
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
        let extract_time = extract_start.elapsed();
        stages_time.push(("data_extraction", extract_time));

        // 4. Генерируем ключ дешифрования
        let key_gen_start = Instant::now();
        let decrypt_key = session.generate_operation_key_for_sequence(self.sequence, "encrypt");
        let key_bytes = decrypt_key.as_bytes();
        let key_gen_time = key_gen_start.elapsed();
        stages_time.push(("key_generation", key_gen_time));

        // 5. Дешифруем с аппаратным ускорением
        let decryption_start = Instant::now();
        if work_buffer.len() < data_len {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Work buffer too small: {} < {}", work_buffer.len(), data_len)
            });
        }

        work_buffer[..data_len].copy_from_slice(&encrypted_data[..data_len]);

        let mut chacha_key = [0u8; 32];
        chacha_key.copy_from_slice(&key_bytes[..32]);

        let mut encrypted_copy = vec![0u8; data_len];
        encrypted_copy.copy_from_slice(&encrypted_data[..data_len]);

        chacha20_accel.encrypt_in_place(
            &chacha_key,
            nonce.try_into().unwrap(),
            0,
            &mut work_buffer[..data_len],
        );
        let decryption_time = decryption_start.elapsed();
        stages_time.push(("decryption", decryption_time));

        // 6. Проверяем TAG
        let tag_check_start = Instant::now();
        let received_tag = &encrypted_data[data_len..data_len + TAG_SIZE];
        let expected_tag = blake3_accel.hash_keyed(&chacha_key, &encrypted_copy);

        if !constant_time_eq(&expected_tag[..TAG_SIZE], received_tag) {
            return Err(ProtocolError::AuthenticationFailed {
                reason: format!("Invalid TAG. Key sequence: {}, nonce: {}",
                                self.sequence, hex::encode(nonce))
            });
        }
        let tag_check_time = tag_check_start.elapsed();
        stages_time.push(("tag_verification", tag_check_time));

        // 7. Копируем результат
        let copy_start = Instant::now();
        let output_len = data_len.min(output.len());
        output[..output_len].copy_from_slice(&work_buffer[..output_len]);
        let copy_time = copy_start.elapsed();
        stages_time.push(("output_copy", copy_time));

        let total_time = total_start.elapsed();

        // Логируем время выполнения каждого этапа
        info!("PACKET DECRYPTION PERFORMANCE:");
        info!("  Total time: {:?} ({:.2} ms)", total_time, total_time.as_micros() as f64 / 1000.0);

        for (stage_name, duration) in &stages_time {
            info!("  {}: {:?} ({:.2} µs, {:.1}%)",
                  stage_name,
                  duration,
                  duration.as_nanos() as f64 / 1000.0,
                  (duration.as_nanos() as f64 / total_time.as_nanos() as f64) * 100.0);
        }

        Ok((self.packet_type, output_len))
    }

    #[inline(always)]
    fn verify_signature_accel(
        &self,
        session: &PhantomSession,
        blake3_accel: &Blake3Accelerator,
    ) -> ProtocolResult<()> {
        let verify_start = Instant::now();

        // Проверяем, что ciphertext содержит хотя бы nonce
        if self.ciphertext.len() < NONCE_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: format!("Ciphertext too short for nonce: {} < {}",
                                 self.ciphertext.len(), NONCE_SIZE)
            });
        }

        let nonce = &self.ciphertext[..NONCE_SIZE];
        let encrypted_data = &self.ciphertext[NONCE_SIZE..];

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

        let expected_signature = blake3_accel.hash_keyed(key_bytes, &input);

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

        let verify_time = verify_start.elapsed();
        debug!("Signature verification time: {:?} ({:.2} µs)", verify_time, verify_time.as_nanos() as f64 / 1000.0);

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
        let process_start = Instant::now();

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

        let process_time = process_start.elapsed();
        info!("Full packet processing time: {:?} ({:.2} ms)",
              process_time, process_time.as_micros() as f64 / 1000.0);

        Ok((packet_type, output_buffer))
    }

    #[inline]
    pub fn create_outgoing_vec(
        &self,
        session: &PhantomSession,
        packet_type: u8,
        plaintext: &[u8],
    ) -> ProtocolResult<Vec<u8>> {
        let create_start = Instant::now();

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

        let create_time = create_start.elapsed();
        info!("Full packet creation time: {:?} ({:.2} ms)",
              create_time, create_time.as_micros() as f64 / 1000.0);

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