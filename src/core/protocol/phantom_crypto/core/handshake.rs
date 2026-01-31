use std::time::{Instant, Duration};
use rand_core::{OsRng, RngCore};
use x25519_dalek::{EphemeralSecret, PublicKey};
use tracing::info;

use crate::core::protocol::error::{ProtocolResult, ProtocolError};
use super::keys::PhantomSession;

/// Константы протокола
pub const CLIENT_HELLO: u8 = 0xA0;
pub const SERVER_HELLO: u8 = 0xA1;
pub const PROTOCOL_VERSION: u8 = 0x02;

/// Результат handshake
pub struct PhantomHandshakeResult {
    pub session: PhantomSession,
    pub role: HandshakeRole,
    pub handshake_time: Duration,
}

#[derive(Debug, Clone, Copy)]
pub enum HandshakeRole {
    Client,
    Server,
}

/// Выполняет handshake с фантомными ключами
pub async fn perform_phantom_handshake(
    stream: &mut tokio::net::TcpStream,
    role: HandshakeRole,
) -> ProtocolResult<PhantomHandshakeResult> {
    let handshake_start = Instant::now();

    let result = match role {
        HandshakeRole::Client => client_phantom_handshake(stream, handshake_start).await,
        HandshakeRole::Server => server_phantom_handshake(stream, handshake_start).await,
    };

    if let Ok(ref res) = result {
        let handshake_time = handshake_start.elapsed();

        info!(
            "Phantom handshake completed in {:?}, session_id: {}",
            handshake_time,
            hex::encode(res.session.session_id())
        );
    }

    result
}

/// Клиентская часть handshake
async fn client_phantom_handshake(
    stream: &mut tokio::net::TcpStream,
    start_time: Instant,
) -> ProtocolResult<PhantomHandshakeResult> {
    info!("Starting client phantom handshake");
    let mut stages_time = Vec::new();

    let mut rng = OsRng;

    // 1. Генерируем клиентские ключи
    let keygen_start = Instant::now();
    let client_secret = EphemeralSecret::random_from_rng(&mut rng);
    let client_pub = PublicKey::from(&client_secret);
    let client_pub_bytes = *client_pub.as_bytes();
    let keygen_time = keygen_start.elapsed();
    stages_time.push(("key_generation", keygen_time));

    // 2. Генерируем nonce
    let nonce_start = Instant::now();
    let mut client_nonce = [0u8; 16];
    rng.fill_bytes(&mut client_nonce);
    let nonce_time = nonce_start.elapsed();
    stages_time.push(("nonce_generation", nonce_time));

    // 3. Отправляем ClientHello
    let send_start = Instant::now();
    let mut client_hello = Vec::with_capacity(50);
    client_hello.push(CLIENT_HELLO);
    client_hello.push(PROTOCOL_VERSION);
    client_hello.extend_from_slice(&client_pub_bytes);
    client_hello.extend_from_slice(&client_nonce);

    crate::core::protocol::packets::frame_writer::write_frame(
        stream,
        &client_hello
    ).await?;
    let send_time = send_start.elapsed();
    stages_time.push(("clienthello_send", send_time));

    // 4. Читаем ServerHello
    let receive_start = Instant::now();
    let server_hello = tokio::time::timeout(
        Duration::from_secs(10),
        crate::core::protocol::packets::frame_reader::read_frame(stream)
    )
        .await
        .map_err(|_| ProtocolError::Timeout { duration: Duration::from_secs(10) })??;
    let receive_time = receive_start.elapsed();
    stages_time.push(("serverhello_receive", receive_time));

    if server_hello.len() != 50 || server_hello[0] != SERVER_HELLO {
        return Err(ProtocolError::HandshakeFailed {
            reason: format!("Invalid ServerHello: {} bytes", server_hello.len())
        });
    }

    if server_hello[1] != PROTOCOL_VERSION {
        return Err(ProtocolError::HandshakeFailed {
            reason: format!("Protocol version mismatch: expected {}, got {}",
                            PROTOCOL_VERSION, server_hello[1])
        });
    }

    // 5. Парсим ServerHello
    let parse_start = Instant::now();
    let server_pub_bytes: [u8; 32] = server_hello[2..34].try_into()
        .map_err(|_| ProtocolError::MalformedPacket {
            details: "Invalid server public key".to_string()
        })?;

    let server_nonce: [u8; 16] = server_hello[34..50].try_into()
        .map_err(|_| ProtocolError::MalformedPacket {
            details: "Invalid server nonce".to_string()
        })?;
    let parse_time = parse_start.elapsed();
    stages_time.push(("serverhello_parse", parse_time));

    // 6. Вычисляем общий секрет
    let dh_start = Instant::now();
    let server_pub = PublicKey::from(server_pub_bytes);
    let shared_secret = client_secret.diffie_hellman(&server_pub);
    let shared_secret_bytes = *shared_secret.as_bytes();
    let dh_time = dh_start.elapsed();
    stages_time.push(("diffie_hellman", dh_time));

    // 7. Создаем фантомную сессию
    let session_start = Instant::now();
    let session = PhantomSession::from_dh_shared(
        &shared_secret_bytes,
        &client_nonce,
        &server_nonce,
        &client_pub_bytes,
        &server_pub_bytes,
    );
    let session_time = session_start.elapsed();
    stages_time.push(("session_creation", session_time));

    let handshake_time = start_time.elapsed();

    // Логируем время выполнения каждого этапа
    info!("CLIENT HANDSHAKE PERFORMANCE:");
    info!("  Total time: {:?} ({:.2} ms)", handshake_time, handshake_time.as_micros() as f64 / 1000.0);

    for (stage_name, duration) in &stages_time {
        info!("  {}: {:?} ({:.2} µs, {:.1}%)", 
              stage_name, 
              duration, 
              duration.as_nanos() as f64 / 1000.0,
              (duration.as_nanos() as f64 / handshake_time.as_nanos() as f64) * 100.0);
    }

    info!(
        "Client phantom handshake completed in {:?}, session_id: {}",
        handshake_time,
        hex::encode(session.session_id())
    );

    Ok(PhantomHandshakeResult {
        session,
        role: HandshakeRole::Client,
        handshake_time,
    })
}

/// Серверная часть handshake
async fn server_phantom_handshake(
    stream: &mut tokio::net::TcpStream,
    start_time: Instant,
) -> ProtocolResult<PhantomHandshakeResult> {
    info!("Starting server phantom handshake");
    let mut stages_time = Vec::new();

    // 1. Читаем ClientHello
    let receive_start = Instant::now();
    let client_hello = crate::core::protocol::packets::frame_reader::read_frame(stream)
        .await?;
    let receive_time = receive_start.elapsed();
    stages_time.push(("clienthello_receive", receive_time));

    if client_hello.len() != 50 || client_hello[0] != CLIENT_HELLO {
        return Err(ProtocolError::HandshakeFailed {
            reason: format!("Invalid ClientHello: {} bytes", client_hello.len())
        });
    }

    if client_hello[1] != PROTOCOL_VERSION {
        return Err(ProtocolError::HandshakeFailed {
            reason: format!("Protocol version mismatch: expected {}, got {}",
                            PROTOCOL_VERSION, client_hello[1])
        });
    }

    // 2. Парсим ClientHello
    let parse_start = Instant::now();
    let client_pub_bytes: [u8; 32] = client_hello[2..34].try_into()
        .map_err(|_| ProtocolError::MalformedPacket {
            details: "Invalid client public key".to_string()
        })?;

    let client_nonce: [u8; 16] = client_hello[34..50].try_into()
        .map_err(|_| ProtocolError::MalformedPacket {
            details: "Invalid client nonce".to_string()
        })?;

    let client_pub = PublicKey::from(client_pub_bytes);
    let parse_time = parse_start.elapsed();
    stages_time.push(("clienthello_parse", parse_time));

    // 3. Генерируем серверные ключи
    let keygen_start = Instant::now();
    let mut rng = OsRng;
    let server_secret = EphemeralSecret::random_from_rng(&mut rng);
    let server_pub = PublicKey::from(&server_secret);
    let server_pub_bytes = *server_pub.as_bytes();
    let keygen_time = keygen_start.elapsed();
    stages_time.push(("key_generation", keygen_time));

    // 4. Генерируем server nonce
    let nonce_start = Instant::now();
    let mut server_nonce = [0u8; 16];
    rng.fill_bytes(&mut server_nonce);
    let nonce_time = nonce_start.elapsed();
    stages_time.push(("nonce_generation", nonce_time));

    // 5. Отправляем ServerHello
    let send_start = Instant::now();
    let mut server_hello = Vec::with_capacity(50);
    server_hello.push(SERVER_HELLO);
    server_hello.push(PROTOCOL_VERSION);
    server_hello.extend_from_slice(&server_pub_bytes);
    server_hello.extend_from_slice(&server_nonce);

    crate::core::protocol::packets::frame_writer::write_frame(
        stream,
        &server_hello
    ).await?;
    let send_time = send_start.elapsed();
    stages_time.push(("serverhello_send", send_time));

    // 6. Вычисляем общий секрет
    let dh_start = Instant::now();
    let shared_secret = server_secret.diffie_hellman(&client_pub);
    let shared_secret_bytes = *shared_secret.as_bytes();
    let dh_time = dh_start.elapsed();
    stages_time.push(("diffie_hellman", dh_time));

    // 7. Создаем фантомную сессию
    let session_start = Instant::now();
    let session = PhantomSession::from_dh_shared(
        &shared_secret_bytes,
        &client_nonce,
        &server_nonce,
        &client_pub_bytes,
        &server_pub_bytes,
    );
    let session_time = session_start.elapsed();
    stages_time.push(("session_creation", session_time));

    let handshake_time = start_time.elapsed();

    // Логируем время выполнения каждого этапа
    info!("SERVER HANDSHAKE PERFORMANCE:");
    info!("  Total time: {:?} ({:.2} ms)", handshake_time, handshake_time.as_micros() as f64 / 1000.0);

    for (stage_name, duration) in &stages_time {
        info!("  {}: {:?} ({:.2} µs, {:.1}%)", 
              stage_name, 
              duration, 
              duration.as_nanos() as f64 / 1000.0,
              (duration.as_nanos() as f64 / handshake_time.as_nanos() as f64) * 100.0);
    }

    info!(
        "Server phantom handshake completed in {:?}, session_id: {}",
        handshake_time,
        hex::encode(session.session_id())
    );

    Ok(PhantomHandshakeResult {
        session,
        role: HandshakeRole::Server,
        handshake_time,
    })
}