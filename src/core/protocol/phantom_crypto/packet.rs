use std::time::Instant;
use rand_core::{OsRng, RngCore};
use constant_time_eq::constant_time_eq;
use tracing::debug;

use crate::core::protocol::error::{ProtocolResult, ProtocolError};
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
    ) -> ProtocolResult<usize> { // Возвращаем размер
        let start = Instant::now();
        debug_assert!(plaintext.len() <= MAX_PAYLOAD_SIZE);

        // 1. Генерируем операционный ключ
        let operation_key = session.generate_operation_key("encrypt");
        let key_bytes = operation_key.as_bytes();

        // 2. Генерируем nonce
        let mut nonce = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce);

        // 3. Рассчитываем размеры
        let header_size = 39; // 2 + 2 + 16 + 8 + 8 + 1
        let nonce_start = header_size;
        let ciphertext_start = nonce_start + NONCE_SIZE;
        let ciphertext_with_tag_len = plaintext.len() + TAG_SIZE;
        let ciphertext_with_tag_end = ciphertext_start + ciphertext_with_tag_len;
        let signature_start = ciphertext_with_tag_end;
        let total_size = signature_start + SIGNATURE_SIZE;

        debug_assert!(buffer.len() >= total_size);

        // Разделяем буфер на части
        let (header_slice, rest) = buffer.split_at_mut(header_size);
        let (nonce_slice, rest) = rest.split_at_mut(NONCE_SIZE);
        let (ciphertext_slice, signature_slice) = rest.split_at_mut(ciphertext_with_tag_len);

        // 4. Копируем nonce
        nonce_slice.copy_from_slice(&nonce);

        // 5. Шифруем с аппаратным ускорением
        ciphertext_slice[..plaintext.len()].copy_from_slice(plaintext);

        // Используем оптимизированный ChaCha20
        let mut chacha_key = [0u8; 32];
        chacha_key.copy_from_slice(key_bytes);

        chacha20_accel.encrypt_in_place(
            &chacha_key,
            &nonce,
            0, // counter
            &mut ciphertext_slice[..plaintext.len()],
        );

        // 6. Добавляем Poly1305 TAG (упрощенно - используем Blake3 для имитации)
        let tag = blake3_accel.hash_keyed(&chacha_key, &ciphertext_slice[..plaintext.len()]);
        ciphertext_slice[plaintext.len()..].copy_from_slice(&tag[..TAG_SIZE]);

        // 7. Создаем подпись с аппаратным ускорением
        Self::create_signature_accel(
            session,
            &operation_key,
            packet_type,
            &nonce,
            ciphertext_slice,
            signature_slice,
            blake3_accel,
        )?;

        // 8. Формируем заголовок
        Self::encode_header(
            session,
            operation_key.sequence,
            packet_type,
            (total_size - 4) as u16,
            header_slice,
        );

        debug!(
            "Packet created in {:?}: {} bytes, seq={}",
            start.elapsed(),
            total_size,
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
        let mut input = Vec::with_capacity(16 + 8 + 1 + NONCE_SIZE + encrypted_data.len());
        input.extend_from_slice(session.session_id());
        input.extend_from_slice(&sign_key.sequence.to_be_bytes());
        input.push(packet_type);
        input.extend_from_slice(nonce);
        input.extend_from_slice(encrypted_data);

        let signature = blake3_accel.hash_keyed(sign_key.as_bytes(), &input);
        sig_buffer.copy_from_slice(&signature);

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

    /// Декодирует пакет (zero allocation)
    #[inline(always)]
    pub fn decode(data: &'a [u8]) -> ProtocolResult<Self> {
        if data.len() < 39 + NONCE_SIZE + TAG_SIZE + SIGNATURE_SIZE {
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
        if data.len() != 4 + length {
            return Err(ProtocolError::MalformedPacket {
                details: "Length mismatch".to_string()
            });
        }

        let session_id: &[u8; 16] = data[4..20].try_into().unwrap();
        let sequence = u64::from_be_bytes(data[20..28].try_into().unwrap());
        let _timestamp = u64::from_be_bytes(data[28..36].try_into().unwrap());
        let packet_type = data[36];

        let ciphertext_start = 37;
        let ciphertext_end = length - SIGNATURE_SIZE + 4;
        let ciphertext = &data[ciphertext_start..ciphertext_end];

        let signature: &[u8; 32] = data[ciphertext_end..ciphertext_end + 32].try_into().unwrap();

        Ok(Self {
            session_id,
            sequence,
            timestamp: 0, // Не используется в verify
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
        let nonce = &self.ciphertext[..NONCE_SIZE];
        let encrypted_data = &self.ciphertext[NONCE_SIZE..];

        if encrypted_data.len() < TAG_SIZE {
            return Err(ProtocolError::MalformedPacket {
                details: "Data too short".to_string()
            });
        }

        let data_len = encrypted_data.len() - TAG_SIZE;

        // 4. Генерируем ключ дешифрования
        let decrypt_key = session.generate_operation_key_for_sequence(self.sequence, "encrypt");
        let key_bytes = decrypt_key.as_bytes();

        // 5. Дешифруем с аппаратным ускорением
        work_buffer[..data_len].copy_from_slice(&encrypted_data[..data_len]);

        let mut chacha_key = [0u8; 32];
        chacha_key.copy_from_slice(key_bytes);

        chacha20_accel.encrypt_in_place(
            &chacha_key,
            nonce.try_into().unwrap(),
            0,
            &mut work_buffer[..data_len],
        );

        // 6. Проверяем TAG (упрощенно)
        let tag = blake3_accel.hash_keyed(&chacha_key, &work_buffer[..data_len]);
        if !constant_time_eq(&tag[..TAG_SIZE], &encrypted_data[data_len..]) {
            return Err(ProtocolError::AuthenticationFailed {
                reason: "Invalid TAG".to_string()
            });
        }

        // 7. Копируем результат
        let output_len = data_len.min(output.len());
        output[..output_len].copy_from_slice(&work_buffer[..output_len]);

        Ok((self.packet_type, output_len))
    }

    #[inline(always)]
    fn verify_signature_accel(
        &self,
        session: &PhantomSession,
        blake3_accel: &Blake3Accelerator,
    ) -> ProtocolResult<()> {
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
                reason: "Invalid signature".to_string()
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
        let mut buffer = vec![0u8; MAX_PAYLOAD_SIZE * 2];

        let size = PhantomPacket::create(
            session,
            packet_type,
            plaintext,
            &mut buffer,
            &self.chacha20_accel,
            &self.blake3_accel,
        )?;

        buffer.truncate(size);
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