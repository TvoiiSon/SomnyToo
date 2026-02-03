use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tracing::{info, error, debug};

use crate::core::protocol::phantom_crypto::core::keys::PhantomSession;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::phantom_crypto::batch::integration::BatchSystem;

pub async fn handle_phantom_client_connection(
    stream: TcpStream,
    peer: std::net::SocketAddr,
    session: Arc<PhantomSession>,
    phantom_session_manager: Arc<PhantomSessionManager>,
    connection_manager: Arc<PhantomConnectionManager>,
    batch_system: Arc<BatchSystem>,
) -> anyhow::Result<()> {
    let session_id = session.session_id();
    info!(target: "server", "💓 Starting batch-integrated phantom connection for session: {} from {}",
        hex::encode(session_id), peer);

    return handle_connection_with_batch(
        stream,
        peer,
        session,
        phantom_session_manager,
        connection_manager,
        batch_system,
    ).await;
}

async fn handle_connection_with_batch(
    stream: TcpStream,
    peer: std::net::SocketAddr,
    session: Arc<PhantomSession>,
    phantom_session_manager: Arc<PhantomSessionManager>,
    connection_manager: Arc<PhantomConnectionManager>,
    batch_system: Arc<BatchSystem>,
) -> anyhow::Result<()> {
    let session_id = session.session_id().to_vec();

    // Разделяем поток на чтение и запись
    let (read_half, write_half) = stream.into_split();

    // Регистрируем соединение в connection manager
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
    connection_manager.register_connection(session_id.clone(), shutdown_tx).await;

    // Используем новый BatchSystem API вместо прямого доступа к batch_reader
    if let Err(e) = batch_system.register_connection(
        peer,
        session_id.clone(),
        Box::new(read_half),
        Box::new(write_half),
    ).await {
        error!("Failed to register connection with batch system: {}", e);
        cleanup_connection(&session_id, &phantom_session_manager, &connection_manager).await;
        return Ok(());
    }

    // Регистрируем сессию в session manager
    phantom_session_manager.register_session(session_id.clone(), session.clone(), peer).await;

    info!("✅ Connection {} fully registered with batch system", peer);

    // ВАЖНОЕ ИЗМЕНЕНИЕ: Ждем небольшое время, чтобы BatchReader начал работу
    debug!("⏳ Waiting for BatchReader to start reading...");
    tokio::time::sleep(Duration::from_millis(50)).await;

    // ЖДЕМ КОМАНДУ НА ЗАВЕРШЕНИЕ ИЛИ ТАЙМАУТ
    tokio::select! {
        _ = shutdown_rx.recv() => {
            info!("👻 Connection {} closed by manager", peer);
        }
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            info!("👻 Connection {} timeout after 30 seconds", peer);
        }
    }

    // Очистка при закрытии соединения
    cleanup_connection(&session_id, &phantom_session_manager, &connection_manager).await;

    Ok(())
}

async fn cleanup_connection(
    session_id: &[u8],
    session_manager: &Arc<PhantomSessionManager>,
    connection_manager: &Arc<PhantomConnectionManager>,
) {
    session_manager.force_remove_session(session_id).await;
    connection_manager.unregister_connection(session_id).await;
}

// Также нужно добавить структуру PhantomConnectionManager если её нет
#[derive(Clone)]
pub struct PhantomConnectionManager {
    active_connections: Arc<tokio::sync::RwLock<std::collections::HashMap<Vec<u8>, tokio::sync::mpsc::Sender<()>>>>,
}

impl PhantomConnectionManager {
    pub fn new() -> Self {
        Self {
            active_connections: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    pub async fn connection_exists(&self, session_id: &[u8]) -> bool {
        let connections = self.active_connections.read().await;
        connections.contains_key(session_id)
    }

    pub async fn register_connection(&self, session_id: Vec<u8>, shutdown_tx: tokio::sync::mpsc::Sender<()>) {
        let mut connections = self.active_connections.write().await;
        connections.insert(session_id.clone(), shutdown_tx);
        info!("👻 Phantom connection registered for session: {}", hex::encode(session_id));
    }

    pub async fn unregister_connection(&self, session_id: &[u8]) {
        let mut connections = self.active_connections.write().await;
        connections.remove(session_id);
        info!("👻 Phantom connection unregistered for session: {}", hex::encode(session_id));
    }

    pub async fn force_disconnect(&self, session_id: &[u8]) {
        if let Some(shutdown_tx) = self.active_connections.write().await.remove(session_id) {
            let _ = shutdown_tx.send(()).await;
            info!("👻 Forced disconnect for phantom session: {}", hex::encode(session_id));
        }
    }

    pub async fn get_active_connections_count(&self) -> usize {
        let connections = self.active_connections.read().await;
        connections.len()
    }
}