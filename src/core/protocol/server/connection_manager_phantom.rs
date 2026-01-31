use std::sync::Arc;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Instant, Duration};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn, debug};

use crate::core::protocol::phantom_crypto::core::keys::PhantomSession;
use crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::server::security::rate_limiter::instance::RATE_LIMITER;
use crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;
use crate::core::protocol::packets::packet_service::PhantomPacketService;
use crate::core::protocol::server::batch_integration::PhantomBatchSystem;

const MAX_PACKET_SIZE: usize = 2 * 1024 * 1024; // 2 MB
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct PhantomConnectionManager {
    active_connections: Arc<RwLock<HashMap<Vec<u8>, mpsc::Sender<()>>>>,
}

impl PhantomConnectionManager {
    pub fn new() -> Self {
        Self {
            active_connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn connection_exists(&self, session_id: &[u8]) -> bool {
        let connections = self.active_connections.read().await;
        connections.contains_key(session_id)
    }

    pub async fn register_connection(&self, session_id: Vec<u8>, shutdown_tx: mpsc::Sender<()>) {
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

pub async fn handle_phantom_client_connection(
    stream: TcpStream,
    peer: SocketAddr,
    session: Arc<PhantomSession>,
    phantom_crypto_pool: Arc<PhantomCrypto>,
    phantom_session_manager: Arc<PhantomSessionManager>,
    connection_manager: Arc<PhantomConnectionManager>,
    heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    packet_service: Arc<PhantomPacketService>,
    _batch_system: Arc<PhantomBatchSystem>, // Пока не используется, но оставляем для будущего
) -> anyhow::Result<()> {
    let session_id = session.session_id();
    info!(target: "server", "💓 Starting heartbeat-integrated phantom connection for session: {} from {}",
        hex::encode(session_id), peer);

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    // Используем старый метод, пока batch система не доработана
    return handle_connection_without_batch(
        stream,
        peer,
        session,
        phantom_crypto_pool,
        phantom_session_manager,
        connection_manager,
        heartbeat_manager,
        packet_service,
    ).await;
}

async fn handle_connection_without_batch(
    stream: TcpStream,
    peer: SocketAddr,
    session: Arc<PhantomSession>,
    phantom_crypto_pool: Arc<PhantomCrypto>,
    phantom_session_manager: Arc<PhantomSessionManager>,
    connection_manager: Arc<PhantomConnectionManager>,
    heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    packet_service: Arc<PhantomPacketService>,
) -> anyhow::Result<()> {
    // Старая реализация без batch системы
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
    let (reader, writer) = stream.into_split();

    connection_manager.register_connection(
        session.session_id().to_vec(),
        shutdown_tx
    ).await;

    let writer_task = tokio::spawn(phantom_write_task(
        writer,
        heartbeat_manager.clone(),
        session.session_id().to_vec(),
        peer,
    ));

    let process_result = tokio::select! {
        result = phantom_process_loop(
            reader,
            peer,
            session.clone(),
            phantom_crypto_pool,
            phantom_session_manager.clone(),
            heartbeat_manager.clone(),
            packet_service.clone(),
        ) => {
            result
        }
        _ = shutdown_rx.recv() => {
            info!(target: "server", "👻 {} forcibly disconnected by timeout", peer);
            Ok(())
        }
    };

    writer_task.abort();
    phantom_session_manager.force_remove_session(session.session_id()).await;
    connection_manager.unregister_connection(session.session_id()).await;

    process_result
}

async fn batch_write_task(
    _batch_system: Arc<PhantomBatchSystem>,
    _heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    _session_id: Vec<u8>,
    _peer: SocketAddr,
) {
    // TODO: Реализовать batch write task
    info!("Batch write task not implemented yet");
}

async fn batch_process_loop(
    _batch_system: Arc<PhantomBatchSystem>,
    _peer: SocketAddr,
    _session: Arc<PhantomSession>,
    _crypto_pool: Arc<PhantomCrypto>,
    _session_manager: Arc<PhantomSessionManager>,
    _heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    _packet_service: Arc<PhantomPacketService>,
) -> anyhow::Result<()> {
    // TODO: Реализовать batch process loop
    info!("Batch process loop not implemented yet");
    Ok(())
}

async fn phantom_write_task(
    writer: tokio::net::tcp::OwnedWriteHalf,
    heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    session_id: Vec<u8>,
    _peer: SocketAddr,
) {
    let mut last_heartbeat_sent = Instant::now();
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

    loop {
        match writer.writable().await {
            Ok(()) => {
                if last_heartbeat_sent.elapsed() >= HEARTBEAT_INTERVAL {
                    if let Err(e) = send_heartbeat(&writer, &session_id).await {
                        warn!("💓 Failed to send heartbeat to session {}: {}",
                            hex::encode(&session_id), e);
                    } else {
                        debug!("💓 Heartbeat sent to session {}", hex::encode(&session_id));
                        last_heartbeat_sent = Instant::now();

                        heartbeat_manager.heartbeat_received(session_id.clone());
                    }
                }

                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => {
                warn!("👻 Phantom write task error for session {}: {}",
                    hex::encode(&session_id), e);
                break;
            }
        }
    }
}

async fn send_heartbeat(
    writer: &tokio::net::tcp::OwnedWriteHalf,
    session_id: &[u8],
) -> anyhow::Result<()> {
    let heartbeat_packet = vec![0x10];

    match writer.try_write(&heartbeat_packet) {
        Ok(_) => {
            debug!("💓 Heartbeat packet sent for session {}", hex::encode(session_id));
            Ok(())
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                Ok(())
            } else {
                Err(anyhow::anyhow!("Failed to send heartbeat: {}", e))
            }
        }
    }
}

async fn phantom_process_loop(
    reader: tokio::net::tcp::OwnedReadHalf,
    peer: SocketAddr,
    session: Arc<PhantomSession>,
    crypto_pool: Arc<PhantomCrypto>,
    session_manager: Arc<PhantomSessionManager>,
    heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    packet_service: Arc<PhantomPacketService>,
) -> anyhow::Result<()> {
    let mut last_activity = Instant::now();
    let session_id = session.session_id().to_vec();

    loop {
        if last_activity.elapsed() > INACTIVITY_TIMEOUT {
            warn!(target: "server", "👻 {} inactive for {:?}, closing connection",
                peer, last_activity.elapsed());

            heartbeat_manager.send_custom_alert(
                crate::core::monitoring::unified_monitor::AlertLevel::Warning,
                "phantom_connection",
                &format!("Inactivity timeout for session {} from {}",
                         hex::encode(&session_id), peer)
            ).await;

            break;
        }

        let mut buffer = vec![0u8; 4096];
        match tokio::time::timeout(Duration::from_secs(5), reader.readable()).await {
            Ok(Ok(())) => {
                match reader.try_read(&mut buffer) {
                    Ok(0) => {
                        info!(target: "server", "👻 Phantom connection {} closed by peer", peer);
                        break;
                    }
                    Ok(n) => {
                        last_activity = Instant::now();
                        buffer.truncate(n);

                        heartbeat_manager.heartbeat_received(session_id.clone());

                        if let Err(e) = handle_phantom_packet(
                            &buffer,
                            peer,
                            &session,
                            &crypto_pool,
                            &session_manager,
                            &heartbeat_manager,
                            &packet_service,
                        ).await {
                            warn!("👻 Failed to handle phantom packet: {}", e);
                        }
                    }
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::WouldBlock {
                            continue;
                        }
                        info!(target: "server", "👻 Phantom connection {} read error: {}", peer, e);
                        break;
                    }
                }
            }
            Ok(Err(e)) => {
                info!(target: "server", "👻 Phantom connection {} error: {}", peer, e);
                break;
            }
            Err(_) => {
                continue;
            }
        }
    }

    Ok(())
}

async fn handle_phantom_packet(
    data: &[u8],
    peer: SocketAddr,
    session: &Arc<PhantomSession>,
    crypto_pool: &Arc<PhantomCrypto>,
    session_manager: &Arc<PhantomSessionManager>,
    heartbeat_manager: &Arc<ConnectionHeartbeatManager>,
    packet_service: &Arc<PhantomPacketService>,
) -> anyhow::Result<()> {
    let start = Instant::now();

    let ip_str = peer.ip().to_string();
    let session_id = session.session_id();

    if !RATE_LIMITER.check_packet(&ip_str, data) {
        warn!("👻 Rate limit exceeded for phantom connection {}", peer);

        heartbeat_manager.send_custom_alert(
            crate::core::monitoring::unified_monitor::AlertLevel::Warning,
            "rate_limit",
            &format!("Rate limit exceeded for session {} from {}",
                     hex::encode(session_id), peer)
        ).await;

        return Ok(());
    }

    if data.len() > MAX_PACKET_SIZE {
        warn!("👻 Oversized phantom packet from {}: {} bytes", peer, data.len());
        return Ok(());
    }

    if data.len() >= 1 && data[0] == 0x10 {
        debug!(target: "phantom_heartbeat",
            "👻 Heartbeat received from {} session: {}",
            peer, hex::encode(session_id));

        session_manager.on_heartbeat_received(session_id).await;

        heartbeat_manager.heartbeat_received(session_id.to_vec());

        let processing_time = start.elapsed();
        debug!("💓 Heartbeat processed for session {} in {:?}", hex::encode(session_id), processing_time);
        return Ok(());
    }

    match crypto_pool.process_packet(session.clone(), data.to_vec()).await {
        Ok((packet_type, plaintext)) => {
            let elapsed = start.elapsed();

            #[cfg(feature = "metrics")]
            metrics::histogram!("phantom.connection.packet_process_time", elapsed.as_micros() as f64);

            debug!(
                "👻 Successfully processed phantom packet from {} in {:?}: type=0x{:02X}, size={} bytes",
                peer, elapsed, packet_type, plaintext.len()
            );

            if let Err(e) = process_decrypted_phantom_payload(
                packet_type,
                plaintext,
                peer,
                session.clone(),
                heartbeat_manager,
                packet_service,
            ).await {
                warn!("👻 Failed to process phantom payload from {}: {}", peer, e);
            }
        }
        Err(e) => {
            warn!("👻 Failed to process phantom packet from {}: {}", peer, e);

            heartbeat_manager.send_custom_alert(
                crate::core::monitoring::unified_monitor::AlertLevel::Warning,
                "decryption_error",
                &format!("Processing failed for session {} from {}: {}",
                         hex::encode(session_id), peer, e)
            ).await;
        }
    }

    Ok(())
}

async fn process_decrypted_phantom_payload(
    packet_type: u8,
    plaintext: Vec<u8>,
    peer: SocketAddr,
    session: Arc<PhantomSession>,
    heartbeat_manager: &Arc<ConnectionHeartbeatManager>,
    packet_service: &Arc<PhantomPacketService>,
) -> anyhow::Result<()> {
    debug!(
        "👻 Processing phantom payload: type=0x{:02X}, size={} bytes, session={}, peer={}",
        packet_type,
        plaintext.len(),
        hex::encode(session.session_id()),
        peer
    );

    match packet_service.process_packet(
        session.clone(),
        packet_type,
        plaintext,
        peer,
    ).await {
        Ok(processing_result) => {
            debug!("👻 Packet processing result: should_encrypt={}, response_size={} bytes",
                   processing_result.should_encrypt, processing_result.response.len());

            // TODO: Отправить ответ через batch writer
        }
        Err(e) => {
            warn!("👻 Packet processing error for session {}: {}",
                  hex::encode(session.session_id()), e);

            heartbeat_manager.send_custom_alert(
                crate::core::monitoring::unified_monitor::AlertLevel::Error,
                "packet_processing_error",
                &format!("Packet processing failed for session {} from {}: {}",
                         hex::encode(session.session_id()), peer, e)
            ).await;
        }
    }

    Ok(())
}

async fn check_heartbeat_health(
    heartbeat_manager: &Arc<ConnectionHeartbeatManager>
) -> anyhow::Result<()> {
    let health = heartbeat_manager.health_check().await;
    if !health {
        return Err(anyhow::anyhow!("Heartbeat system health check failed"));
    }

    debug!("💓 Heartbeat system health check passed");
    Ok(())
}