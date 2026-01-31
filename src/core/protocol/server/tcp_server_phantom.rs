use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{info, error};

use crate::core::protocol::phantom_crypto::core::handshake::{perform_phantom_handshake, HandshakeRole};
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::server::connection_manager_phantom::PhantomConnectionManager;
use crate::core::protocol::server::batch_integration::PhantomBatchSystem;

pub async fn handle_phantom_connection(
    mut stream: TcpStream,
    peer: std::net::SocketAddr,
    _phantom_config: crate::config::PhantomConfig,
    session_manager: Arc<PhantomSessionManager>,
    connection_manager: Arc<PhantomConnectionManager>,
    _crypto_pool: Arc<crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto>,
    _heartbeat_manager: Arc<crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager>,
    _packet_service: Arc<crate::core::protocol::packets::packet_service::PhantomPacketService>,
    batch_system: Arc<PhantomBatchSystem>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!("👻 Handling phantom connection from {}", peer);

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

    // Регистрируем сессию
    if let Ok(_) = session_manager.add_session_with_addr(&session_id, session.clone(), peer).await {
        info!("Session registered with address");
    } else {
        session_manager.add_session(&session_id, session.clone()).await;
    }

    // Регистрируем соединение в batch системе
    let (read_half, write_half) = stream.into_split();

    // Регистрируем для batch чтения
    if let Err(e) = batch_system.batch_reader.register_connection(
        peer,
        session_id.clone(),
        Box::new(read_half),
    ).await {
        error!("Failed to register connection with batch reader: {}", e);
        session_manager.force_remove_session(&session_id).await;
        return Ok(());
    }

    // Регистрируем для batch записи
    if let Err(e) = batch_system.batch_writer.register_connection(
        peer,
        session_id.clone(),
        Box::new(write_half),
    ).await {
        error!("Failed to register connection with batch writer: {}", e);
        session_manager.force_remove_session(&session_id).await;
        return Ok(());
    }

    // Регистрируем в connection manager для управления соединением
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
    connection_manager.register_connection(session_id.clone(), shutdown_tx).await;

    info!("✅ Phantom connection fully registered with batch system: {}", peer);

    // Ждем команду на отключение или таймаут
    tokio::select! {
        _ = shutdown_rx.recv() => {
            info!("Connection {} closed by manager", peer);
        }
        _ = tokio::time::sleep(Duration::from_secs(300)) => {
            info!("Connection {} timeout", peer);
        }
    }

    // Удаляем сессию
    session_manager.force_remove_session(&session_id).await;
    connection_manager.unregister_connection(&session_id).await;

    info!("👻 Phantom connection with {} closed", peer);
    Ok(())
}