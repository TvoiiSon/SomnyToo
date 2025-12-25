use aes_gcm::{
    aead::{Aead, Payload}
};
use generic_array::GenericArray;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use rand::RngCore;
use rand_core::OsRng;
use tokio::task;

use crate::core::protocol::crypto::key_manager::session_keys::SessionKeys;

const HEADER_MAGIC: [u8;2] = [0xAB,0xCD];
const SIGNATURE_SIZE: usize = 32;
const NONCE_SIZE: usize = 12;

type HmacSha256 = Hmac<Sha256>;

pub struct PacketBuilder;

impl PacketBuilder {
    pub async fn build_encrypted_packet(
        ctx: &SessionKeys,
        packet_type: u8,
        plaintext: &[u8]
    ) -> Vec<u8> {
        // Копируем все необходимые данные для передачи в blocking task
        let ctx_clone = ctx.clone();
        let plaintext_clone = plaintext.to_vec();
        let packet_type_clone = packet_type;

        task::spawn_blocking(move || {
            // Используем клонированные данные внутри замыкания
            let mut nonce_bytes = [0u8; NONCE_SIZE];
            OsRng.fill_bytes(&mut nonce_bytes);

            // Определяем AAD
            let ciphertext_len = plaintext_clone.len() + 16;
            let total_len: u16 = (1 + NONCE_SIZE + ciphertext_len + SIGNATURE_SIZE) as u16;

            // Создаём выходной вектор
            let mut out = Vec::with_capacity(2 + 2 + total_len as usize);
            out.extend_from_slice(&HEADER_MAGIC);
            out.extend_from_slice(&total_len.to_be_bytes());
            out.push(packet_type_clone);

            // Определяем AAD для AES-GCM
            let aad_start = 0;
            let aad_end = out.len();
            let aad = &out[aad_start..aad_end];

            // Шифруем данные AES-GCM (используем клонированный контекст)
            let nonce = GenericArray::from_slice(&nonce_bytes);
            let ciphertext = ctx_clone.aead_cipher.encrypt(
                nonce,
                Payload {
                    msg: &plaintext_clone, // используем клонированные данные
                    aad
                }
            ).expect("encryption failed");

            // Добавляем nonce и ciphertext
            out.extend_from_slice(&nonce_bytes);
            out.extend_from_slice(&ciphertext);

            // Вычисляем HMAC по всему пакету (используем клонированный контекст)
            let mut mac = HmacSha256::new_from_slice(&ctx_clone.sign_key)
                .expect("bad hmac key");
            mac.update(&out);
            let tag = mac.finalize().into_bytes();
            out.extend_from_slice(&tag);

            out
        }).await.expect("encryption task failed")
    }
}