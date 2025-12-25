use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tracing::{info, warn, error};
use prometheus::Registry;

use crate::core::protocol::packets::processor::dispatcher::Dispatcher;
use crate::core::protocol::server::security::rate_limiter::instance::RATE_LIMITER;
use crate::core::protocol::server::security::security_metrics::SecurityMetrics;
use crate::core::protocol::server::session_manager::SessionManager;
use crate::core::protocol::server::connection_manager::ConnectionManager;
use crate::core::protocol::crypto::handshake::handshake::{perform_handshake, HandshakeRole};

pub async fn handle_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    dispatcher: Arc<Dispatcher>,
    session_manager: Arc<SessionManager>,
    connection_manager: Arc<ConnectionManager>,
) -> anyhow::Result<()> {
    info!(target: "server", "{} connected", peer);
    SecurityMetrics::active_connections().inc();

    // Проверка rate limiting
    if !RATE_LIMITER.check_packet(&peer.ip().to_string(), &[]) {
        warn!(target: "server", "Rate limit exceeded for {}, closing connection", peer);
        SecurityMetrics::active_connections().dec();
        return Ok(());
    }

    // Handshake с таймаутом
    let handshake_result = match timeout(
        Duration::from_secs(30),
        perform_handshake(&mut stream, HandshakeRole::Server)
    ).await {
        Ok(Ok(result)) => {
            // SecurityAudit::log_handshake_success(peer, result.session_keys.session_id).await;
            result
        },
        Ok(Err(e)) => {
            warn!(target: "server", "Handshake failed for {}: {}", peer, e);
            // SecurityAudit::log_handshake_failure(peer, e.to_string()).await;
            SecurityMetrics::active_connections().dec();
            return Ok(());
        }
        Err(_) => {
            error!(target: "server", "Handshake timeout for {}", peer);
            SecurityMetrics::active_connections().dec();
            return Ok(());
        }
    };

    // Проверяем сессию на повторное использование
    if SessionManager::check_session_reuse(&handshake_result.session_keys.session_id).await {
        warn!(target: "server", "Session reuse detected for {}: {}", peer, hex::encode(handshake_result.session_keys.session_id));
        // SecurityAudit::log_session_reuse(peer, handshake_result.session_keys.session_id).await;
        SecurityMetrics::active_connections().dec();
        return Ok(());
    }

    // Получаем heartbeat_manager из session_manager
    let heartbeat_manager = session_manager.get_heartbeat_manager();

    // Основная обработка соединения
    let result = super::connection_manager::handle_client_connection(
        stream,
        peer,
        Arc::new(handshake_result.session_keys.clone()),
        dispatcher,
        session_manager.clone(),
        heartbeat_manager,
        connection_manager,
    ).await;

    SecurityMetrics::active_connections().dec();
    result
}

pub fn register_metrics(registry: &Registry) -> anyhow::Result<()> {
    SecurityMetrics::register(registry)
}