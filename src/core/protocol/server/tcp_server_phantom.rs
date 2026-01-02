use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration, Instant};
use tracing::{info, warn, error};

use crate::core::protocol::server::security::rate_limiter::instance::RATE_LIMITER;
use crate::core::protocol::server::security::security_metrics::SecurityMetrics;
use crate::core::protocol::phantom_crypto::handshake::{perform_phantom_handshake, HandshakeRole};

pub async fn handle_phantom_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    _dispatcher: Arc<crate::core::protocol::packets::processor::dispatcher::Dispatcher>,
    _session_manager: Arc<crate::core::protocol::server::session_manager::SessionManager>,
    _connection_manager: Arc<crate::core::protocol::server::connection_manager::ConnectionManager>,
) -> anyhow::Result<()> {
    let connection_start = Instant::now();
    info!(target: "server", "{} connected (phantom)", peer);
    SecurityMetrics::active_connections().inc();

    // Проверка rate limiting
    let rate_limit_start = Instant::now();
    if !RATE_LIMITER.check_packet(&peer.ip().to_string(), &[]) {
        warn!(target: "server", "Rate limit exceeded for {}, closing connection", peer);
        SecurityMetrics::active_connections().dec();
        return Ok(());
    }
    let rate_limit_time = rate_limit_start.elapsed();

    // Phantom handshake с таймаутом
    let handshake_start = Instant::now();
    let handshake_result = match timeout(
        Duration::from_secs(30),
        perform_phantom_handshake(&mut stream, HandshakeRole::Server)
    ).await {
        Ok(Ok(result)) => {
            let handshake_time = handshake_start.elapsed();
            info!(target: "server", "Phantom handshake successful for {} in {:?}, session: {}",
                  peer, handshake_time, hex::encode(&result.session.session_id()));
            result
        },
        Ok(Err(e)) => {
            let handshake_time = handshake_start.elapsed();
            warn!(target: "server", "Phantom handshake failed for {} after {:?}: {}",
                  peer, handshake_time, e);
            SecurityMetrics::active_connections().dec();
            return Ok(());
        }
        Err(_) => {
            error!(target: "server", "Phantom handshake timeout for {}", peer);
            SecurityMetrics::active_connections().dec();
            return Ok(());
        }
    };

    info!("Phantom connection setup completed in {:?} (rate limit: {:?}, handshake: {:?})",
          connection_start.elapsed(), rate_limit_time, handshake_start.elapsed());

    // Здесь должна быть логика обработки сессии
    // Для примера просто логируем успешное подключение
    info!("Phantom session established for {}: {}", peer, hex::encode(handshake_result.session.session_id()));

    let total_connection_time = connection_start.elapsed();
    info!(target: "server", "{} phantom connection closed after {:?}", peer, total_connection_time);

    SecurityMetrics::active_connections().dec();
    Ok(())
}

pub fn register_phantom_metrics(registry: &prometheus::Registry) -> anyhow::Result<()> {
    SecurityMetrics::register(registry)
}