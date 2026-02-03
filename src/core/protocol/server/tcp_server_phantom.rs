use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{info, error};

use crate::core::protocol::phantom_crypto::core::handshake::{perform_phantom_handshake, HandshakeRole};
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::server::connection_manager_phantom::PhantomConnectionManager;
use crate::core::protocol::phantom_crypto::batch::integration::BatchSystem;

// Добавляем импорт функции из connection_manager
use crate::core::protocol::server::connection_manager_phantom::handle_phantom_client_connection;

pub async fn handle_phantom_connection(
    mut stream: TcpStream,
    peer: std::net::SocketAddr,
    session_manager: Arc<PhantomSessionManager>,
    connection_manager: Arc<PhantomConnectionManager>,
    batch_system: Arc<BatchSystem>,  // Изменён тип
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!("👻 Handling phantom connection from {}", peer);

    // Выполняем handshake
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
    if let Err(e) = session_manager.add_session_with_addr(&session_id, session.clone(), peer).await {
        error!("Failed to register session: {}", e);
        return Ok(());
    }

    // Используем новую batch-интегрированную функцию
    match handle_phantom_client_connection(
        stream,
        peer,
        session,
        session_manager.clone(),
        connection_manager.clone(),
        batch_system.clone(),
    ).await {
        Ok(()) => {
            info!("✅ Connection {} processed successfully", peer);
            Ok(())
        }
        Err(e) => {
            error!("❌ Connection {} failed: {}", peer, e);
            // Все равно возвращаем Ok, чтобы не падать на одной ошибке соединения
            Ok(())
        }
    }
}