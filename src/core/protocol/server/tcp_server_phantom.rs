use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{info, error, debug};

use crate::core::protocol::phantom_crypto::core::handshake::{perform_phantom_handshake, HandshakeRole};
use crate::core::protocol::packets::decoder::frame_reader;
use crate::core::protocol::packets::encoder::frame_writer;
use crate::core::protocol::packets::processor::dispatcher::Dispatcher;
use crate::core::protocol::crypto::crypto_pool_phantom::PhantomCryptoPool;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::server::connection_manager_phantom::PhantomConnectionManager;
use crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;
use crate::core::protocol::packets::processor::packet_service::PhantomPacketService;

pub async fn handle_phantom_connection(
    mut stream: TcpStream,
    peer: std::net::SocketAddr,
    _phantom_config: crate::config::PhantomConfig,
    session_manager: Arc<PhantomSessionManager>,
    _connection_manager: Arc<PhantomConnectionManager>,
    crypto_pool: Arc<PhantomCryptoPool>,
    _heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    packet_service: Arc<PhantomPacketService>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!("👻 Handling phantom connection from {}", peer);

    // Выполняем handshake с таймаутом
    let handshake_result = match timeout(
        Duration::from_secs(10),
        perform_phantom_handshake(&mut stream, HandshakeRole::Server)
    ).await {
        Ok(result) => result,
        Err(_) => {
            error!("Handshake timeout for {}", peer);
            return Ok(());
        }
    };

    let handshake_result = match handshake_result {
        Ok(result) => result,
        Err(e) => {
            error!("Handshake failed for {}: {}", peer, e);
            return Ok(());
        }
    };

    let session = Arc::new(handshake_result.session);
    let session_id = session.session_id().to_vec();

    info!("✅ Phantom handshake completed for {} session: {}", 
          peer, hex::encode(&session_id));

    // Используем метод с адресом (если он есть) или обычный
    // Вариант 1: Если есть метод с адресом
    if let Ok(_) = session_manager.add_session_with_addr(&session_id, session.clone(), peer).await {
        info!("Session registered with address");
    }
    // Вариант 2: Если нужно использовать старый метод
    else {
        session_manager.add_session(&session_id, session.clone()).await;
    }

    // Создаем диспетчер
    let dispatcher = Arc::new(Dispatcher::spawn(
        4,
        crypto_pool.clone(),
        packet_service.clone(),
    ));

    // Основной цикл обработки пакетов
    loop {
        let frame_result = match timeout(
            Duration::from_secs(30),
            frame_reader::read_frame(&mut stream)
        ).await {
            Ok(result) => result,
            Err(_) => {
                debug!("Read timeout for {}, closing connection", peer);
                break;
            }
        };

        let frame = match frame_result {
            Ok(frame) => frame,
            Err(e) => {
                error!("Failed to read frame from {}: {}", peer, e);
                break;
            }
        };

        if frame.is_empty() {
            info!("Connection closed by {} (empty frame)", peer);
            break;
        }

        debug!("Received {} bytes from {}", frame.len(), peer);

        // Сохраняем копию для определения приоритета
        let frame_copy = frame.clone();

        let (tx, rx) = tokio::sync::oneshot::channel();

        let work = crate::core::protocol::packets::processor::dispatcher::Work {
            ctx: session.clone(),
            raw_payload: frame,
            client_ip: peer,
            reply: tx,
            received_at: tokio::time::Instant::now(),
            priority: crate::core::protocol::packets::processor::priority::determine_priority(&frame_copy),
        };

        if let Err(e) = dispatcher.submit(work).await {
            error!("Failed to submit work to dispatcher for {}: {}", peer, e);
            continue;
        }

        let response = match timeout(Duration::from_secs(5), rx).await {
            Ok(result) => match result {
                Ok(response) => response,
                Err(_) => {
                    error!("Failed to receive response for {}", peer);
                    continue;
                }
            },
            Err(_) => {
                debug!("Response timeout for {}", peer);
                continue;
            }
        };

        match timeout(
            Duration::from_secs(5),
            frame_writer::write_frame(&mut stream, &response)
        ).await {
            Ok(result) => {
                if let Err(e) = result {
                    error!("Failed to write frame to {}: {}", peer, e);
                    break;
                }
            }
            Err(_) => {
                debug!("Write timeout for {}", peer);
                break;
            }
        }

        debug!("Sent {} bytes to {}", response.len(), peer);
    }

    // Очистка - используем существующие методы
    session_manager.force_remove_session(&session_id).await;

    info!("👻 Phantom connection with {} closed", peer);
    Ok(())
}