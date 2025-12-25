use anyhow::{anyhow, Result};
use rand_core::{OsRng, RngCore};
use x25519_dalek::{EphemeralSecret, PublicKey};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tracing::info;
use constant_time_eq::constant_time_eq;
use std::hint::black_box;
use std::time::Duration;

use crate::core::protocol::crypto::key_manager::session_keys::SessionKeys;
use crate::core::protocol::crypto::key_manager::psk_manager::{get_psk, derive_psk_keys};
use crate::core::protocol::packets::decoder::frame_reader::read_frame;
use crate::core::protocol::packets::encoder::frame_writer::write_frame;
use crate::core::protocol::error::{ProtocolResult, ProtocolError, CryptoError};

pub const CLIENT_HELLO: u8 = 0xA0;
pub const SERVER_HELLO: u8 = 0xA1;
pub const PROTOCOL_VERSION: u8 = 0x01;

type HmacSha256 = Hmac<Sha256>;

pub struct HandshakeResult {
    pub session_keys: SessionKeys,
    pub role: HandshakeRole,
}

#[derive(Debug, Clone, Copy)]
pub enum HandshakeRole {
    Client,
    Server,
}

/// Универсальная функция handshake, которая работает для обеих сторон
pub async fn perform_handshake(
    stream: &mut tokio::net::TcpStream,
    role: HandshakeRole,
) -> ProtocolResult<HandshakeResult> {
    match role {
        HandshakeRole::Client => client_handshake(stream).await,
        HandshakeRole::Server => {
            // Теперь можно использовать просто ? так как есть From<anyhow::Error>
            let session_keys = server_handshake(stream).await?;
            Ok(HandshakeResult {
                session_keys,
                role: HandshakeRole::Server,
            })
        }
    }
}

/// Клиентская часть handshake
async fn client_handshake(stream: &mut tokio::net::TcpStream) -> ProtocolResult<HandshakeResult> {
    let psk_bytes = get_psk()
        .map_err(|e| ProtocolError::from(e))?; // Используем автоматическую конвертацию

    let (client_auth_key, server_auth_key) = derive_psk_keys(&psk_bytes)
        .map_err(|_e| ProtocolError::Crypto { source: CryptoError::KeyDerivationFailed })?;

    // Генерируем клиентские ключи
    let mut rng = OsRng;
    let client_secret = EphemeralSecret::random_from_rng(&mut rng);
    let client_pub = PublicKey::from(&client_secret);

    let mut client_nonce = [0u8; 16];
    rng.fill_bytes(&mut client_nonce);

    let client_hmac = compute_client_hmac(client_pub.as_bytes(), &client_nonce, &client_auth_key);

    // Отправляем ClientHello
    let mut client_hello = Vec::with_capacity(82);
    client_hello.push(CLIENT_HELLO);
    client_hello.push(PROTOCOL_VERSION);
    client_hello.extend_from_slice(client_pub.as_bytes());
    client_hello.extend_from_slice(&client_nonce);
    client_hello.extend_from_slice(&client_hmac);

    // ИСПРАВЛЕНИЕ: Конвертируем anyhow::Error в ProtocolError
    write_frame(stream, &client_hello).await?;

    // Читаем ServerHello с таймаутом
    let server_hello = tokio::time::timeout(
        Duration::from_secs(10),
        read_frame(stream)
    )
        .await
        .map_err(|_| ProtocolError::Timeout { duration: Duration::from_secs(10) })??; // Двойной ? для timeout + read_frame

    if server_hello.len() != 82 || server_hello[0] != SERVER_HELLO {
        // ИСПРАВЛЕНИЕ: Сначала создаем ошибку, потом логируем
        let error = ProtocolError::HandshakeFailed {
            reason: "Invalid ServerHello format".to_string()
        }.log();
        return Err(error);
    }

    // Проверяем версию протокола
    if server_hello[1] != PROTOCOL_VERSION {
        let error = ProtocolError::HandshakeFailed {
            reason: format!("Unsupported protocol version: {}", server_hello[1])
        }.log();
        return Err(error);
    }

    let server_pub_bytes: [u8; 32] = server_hello[2..34].try_into()
        .map_err(|_| ProtocolError::MalformedPacket {
            details: "Invalid server public key length".to_string()
        })?;

    let server_nonce: [u8; 16] = server_hello[34..50].try_into()
        .map_err(|_| ProtocolError::MalformedPacket {
            details: "Invalid server nonce".to_string()
        })?;

    let server_hmac: [u8; 32] = server_hello[50..82].try_into()
        .map_err(|_| ProtocolError::MalformedPacket {
            details: "Invalid server HMAC".to_string()
        })?;

    // Проверяем валидность публичного ключа сервера
    if !is_valid_public_key(&server_pub_bytes) {
        let error = ProtocolError::HandshakeFailed {
            reason: "Invalid server public key".to_string()
        }.log();
        return Err(error);
    }

    // Проверяем HMAC сервера
    verify_server_authentication(&server_pub_bytes, &server_nonce, &server_hmac, &server_auth_key)
        .map_err(|e| ProtocolError::AuthenticationFailed { reason: e.to_string() })?;

    let server_pub = PublicKey::from(server_pub_bytes);
    let shared = client_secret.diffie_hellman(&server_pub);

    // Генерируем session keys
    let salt = create_salt(client_pub.as_bytes(), &server_pub_bytes, &client_nonce, &server_nonce);
    let session_keys = SessionKeys::from_dh_shared_with_psk(shared.as_bytes(), &salt, &psk_bytes);

    info!("Client handshake completed successfully");

    Ok(HandshakeResult {
        session_keys,
        role: HandshakeRole::Client,
    })
}

/// Серверная часть handshake
async fn server_handshake(stream: &mut tokio::net::TcpStream) -> ProtocolResult<SessionKeys> {
    let psk_bytes = get_psk().map_err(ProtocolError::from)?; // Автоматическая конвертация

    let (client_auth_key, server_auth_key) = derive_psk_keys(&psk_bytes)
        .map_err(|_e| ProtocolError::Crypto { source: CryptoError::KeyDerivationFailed })?;

    // Читаем ClientHello
    let hello = read_frame(stream).await?; // Конвертируем anyhow::Error

    if hello.len() != 82 || hello[0] != CLIENT_HELLO {
        let error = ProtocolError::HandshakeFailed {
            reason: format!("Bad ClientHello: expected 82 bytes, got {}", hello.len())
        }.log();
        return Err(error);
    }

    // Проверяем версию протокола
    if hello[1] != PROTOCOL_VERSION {
        let error = ProtocolError::HandshakeFailed {
            reason: format!("Unsupported protocol version: {}", hello[1])
        }.log();
        return Err(error);
    }

    let client_pub_bytes: [u8; 32] = hello[2..34].try_into()
        .map_err(|_| ProtocolError::MalformedPacket {
            details: "Invalid client public key length".to_string()
        })?;

    let client_nonce: [u8; 16] = hello[34..50].try_into()
        .map_err(|_| ProtocolError::MalformedPacket {
            details: "Invalid client nonce".to_string()
        })?;

    let client_hmac: [u8; 32] = hello[50..82].try_into()
        .map_err(|_| ProtocolError::MalformedPacket {
            details: "Invalid client HMAC".to_string()
        })?;

    let client_pub = PublicKey::from(client_pub_bytes);
    if !is_valid_public_key(&client_pub_bytes) {
        let error = ProtocolError::HandshakeFailed {
            reason: "Invalid client public key".to_string()
        }.log();
        return Err(error);
    }

    verify_client_authentication(&client_pub_bytes, &client_nonce, &client_hmac, &client_auth_key)
        .map_err(|e| ProtocolError::AuthenticationFailed { reason: e.to_string() })?;

    // Генерируем серверные ключи
    let mut rng = OsRng;
    let server_secret = EphemeralSecret::random_from_rng(&mut rng);
    let server_pub = PublicKey::from(&server_secret);

    let mut server_nonce = [0u8; 16];
    rng.fill_bytes(&mut server_nonce);

    let shared = server_secret.diffie_hellman(&client_pub);
    let salt = create_salt(&client_pub_bytes, server_pub.as_bytes(), &client_nonce, &server_nonce);
    let server_hmac = compute_server_hmac(server_pub.as_bytes(), &server_nonce, &server_auth_key);

    let mut server_hello = Vec::with_capacity(82);
    server_hello.push(SERVER_HELLO);
    server_hello.push(PROTOCOL_VERSION);
    server_hello.extend_from_slice(server_pub.as_bytes());
    server_hello.extend_from_slice(&server_nonce);
    server_hello.extend_from_slice(&server_hmac);

    write_frame(stream, &server_hello).await?;

    info!("Server handshake completed successfully");

    Ok(SessionKeys::from_dh_shared_with_psk(shared.as_bytes(), &salt, &psk_bytes))
}

/// Вычисляет HMAC для клиента (аналогично серверной версии)
fn compute_client_hmac(client_pub: &[u8], client_nonce: &[u8; 16], auth_key: &[u8]) -> [u8; 32] {
    let mut auth_data = Vec::with_capacity(32 + 16 + 2);
    auth_data.extend_from_slice(&(client_pub.len() as u16).to_be_bytes());
    auth_data.extend_from_slice(client_pub);
    auth_data.extend_from_slice(&(client_nonce.len() as u16).to_be_bytes());
    auth_data.extend_from_slice(client_nonce);

    compute_hmac(&auth_data, auth_key)
}

/// Проверяет аутентификацию сервера (аналогично клиентской версии)
fn verify_server_authentication(
    server_pub: &[u8; 32],
    server_nonce: &[u8; 16],
    received_hmac: &[u8; 32],
    auth_key: &[u8],
) -> ProtocolResult<()> {
    let mut auth_data = Vec::with_capacity(32 + 16 + 2);
    auth_data.extend_from_slice(&(server_pub.len() as u16).to_be_bytes());
    auth_data.extend_from_slice(server_pub);
    auth_data.extend_from_slice(&(server_nonce.len() as u16).to_be_bytes());
    auth_data.extend_from_slice(server_nonce);

    let expected_hmac = compute_hmac(&auth_data, auth_key);

    if !constant_time_eq(&expected_hmac, received_hmac) {
        black_box(compute_hmac(b"dummy", auth_key));
        return Err(ProtocolError::AuthenticationFailed {
            reason: "Server authentication failed".to_string()
        });
    }

    Ok(())
}

fn create_salt(client_pub: &[u8], server_pub: &[u8], client_nonce: &[u8; 16], server_nonce: &[u8; 16]) -> Vec<u8> {
    let mut salt = Vec::with_capacity(80);
    salt.extend_from_slice(client_pub);
    salt.extend_from_slice(server_pub);
    salt.extend_from_slice(client_nonce);
    salt.extend_from_slice(server_nonce);
    salt
}

/// Проверяет аутентификацию клиента
fn verify_client_authentication(client_pub: &[u8; 32], client_nonce: &[u8; 16],
                                received_hmac: &[u8; 32], auth_key: &[u8]) -> Result<()> {
    let mut auth_data = Vec::with_capacity(32 + 16 + 2);
    auth_data.extend_from_slice(&(client_pub.len() as u16).to_be_bytes());
    auth_data.extend_from_slice(client_pub);
    auth_data.extend_from_slice(&(client_nonce.len() as u16).to_be_bytes());
    auth_data.extend_from_slice(client_nonce);

    let expected_hmac = compute_hmac(&auth_data, auth_key);

    if !constant_time_eq(&expected_hmac, received_hmac) {
        black_box(compute_hmac(b"dummy", auth_key));
        return Err(anyhow!("client authentication failed"));
    }

    Ok(())
}

/// Вычисляет HMAC для сервера
fn compute_server_hmac(server_pub: &[u8], server_nonce: &[u8; 16], auth_key: &[u8]) -> [u8; 32] {
    let mut auth_data = Vec::with_capacity(32 + 16 + 2);
    auth_data.extend_from_slice(&(server_pub.len() as u16).to_be_bytes());
    auth_data.extend_from_slice(server_pub);
    auth_data.extend_from_slice(&(server_nonce.len() as u16).to_be_bytes());
    auth_data.extend_from_slice(server_nonce);

    compute_hmac(&auth_data, auth_key)
}

/// Вычисляет HMAC-SHA256 с использованием ключа аутентификации
fn compute_hmac(data: &[u8], auth_key: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(auth_key).expect("Auth key length is valid");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Проверяет валидность публичного ключа на curve25519
fn is_valid_public_key(public_key: &[u8; 32]) -> bool {
    if public_key.iter().all(|&b| b == 0) {
        return false;
    }
    PublicKey::from(*public_key).as_bytes() == public_key
}
