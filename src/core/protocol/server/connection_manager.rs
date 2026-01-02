use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{Instant, Duration};
use tracing::{info, warn, debug, trace};
use tokio::time;

use crate::core::protocol::packets::processor::dispatcher::{Dispatcher, Work};
use crate::core::protocol::crypto::key_manager::session_keys::SessionKeys;
use crate::core::protocol::packets::encoder::frame_writer::write_frame;
use crate::core::protocol::packets::decoder::frame_reader::read_frame;
use crate::core::protocol::server::heartbeat::manager::HeartbeatManager;
use crate::core::protocol::server::session_manager::SessionManager;
use crate::core::protocol::server::security::security_metrics::SecurityMetrics;
use crate::core::protocol::server::security::security_audit::SecurityAudit;
use crate::core::protocol::server::security::rate_limiter::instance::RATE_LIMITER;

const LARGE_THRESHOLD: usize = 16 * 1024;
const MAX_PACKET_SIZE: usize = 2 * 1024 * 1024;
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(60); // 1 минута

#[derive(Clone)]
pub struct ConnectionManager {
    active_connections: Arc<RwLock<HashMap<Vec<u8>, mpsc::Sender<()>>>>,
}

#[derive(Debug)]
pub struct ConnectionStats {
    pub total_connections: usize,
    pub session_ids: Vec<Vec<u8>>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            active_connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn connection_exists(&self, session_id: &[u8]) -> bool {
        let connections = self.active_connections.read().await;
        connections.contains_key(session_id)
    }

    // Метод для получения статистики соединений
    pub async fn get_connection_stats(&self) -> ConnectionStats {
        let connections = self.active_connections.read().await;
        ConnectionStats {
            total_connections: connections.len(),
            session_ids: connections.keys().cloned().collect(),
        }
    }

    pub async fn register_connection(&self, session_id: Vec<u8>, shutdown_tx: mpsc::Sender<()>) {
        let session_id_clone = session_id.clone(); // Клонируем для логирования
        let mut connections = self.active_connections.write().await;
        connections.insert(session_id, shutdown_tx);
        info!("Connection registered for session: {}", hex::encode(&session_id_clone));
    }

    pub async fn unregister_connection(&self, session_id: &[u8]) {
        let mut connections = self.active_connections.write().await;
        connections.remove(session_id);
        info!("Connection unregistered for session: {}", hex::encode(session_id));
    }

    pub async fn force_disconnect(&self, session_id: &[u8]) {
        if let Some(shutdown_tx) = self.active_connections.write().await.remove(session_id) {
            let _ = shutdown_tx.send(()).await;
            info!("Forced disconnect for session: {}", hex::encode(session_id));
        }
    }
}

pub async fn handle_client_connection(
    stream: TcpStream,
    peer: SocketAddr,
    session_keys: Arc<SessionKeys>,
    dispatcher: Arc<Dispatcher>,
    session_manager: Arc<SessionManager>,
    heartbeat_manager: Arc<HeartbeatManager>,
    _connection_manager: Arc<ConnectionManager>,
) -> anyhow::Result<()> {
    // Инициализация SecurityAudit и SecurityMetrics
    let _ = SecurityAudit::initialize().await;

    let timer = SecurityMetrics::processing_time().start_timer();
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(32768);
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
    let (reader, writer) = stream.into_split();

    // Регистрируем соединение
    _connection_manager.register_connection(
        session_keys.session_id.to_vec(),
        shutdown_tx
    ).await;

    let rate_limiter_cleanup = Arc::clone(&RATE_LIMITER);
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(30)); // Очистка каждые 30 секунд
        loop {
            interval.tick().await;
            rate_limiter_cleanup.cleanup_old_history();
        }
    });

    // Регистрируем сессию
    session_manager.register_session(
        session_keys.session_id.to_vec(),
        Arc::clone(&session_keys),
        peer,
    ).await;

    // Writer task
    let writer_task = tokio::spawn(write_task(writer, out_rx));

    // Main processing loop с поддержкой принудительного закрытия
    let process_result = tokio::select! {
        result = process_loop(
            reader,
            peer,
            session_keys.clone(),
            dispatcher,
            out_tx.clone(),
            session_manager.clone(),
        ) => {
            result
        }
        _ = shutdown_rx.recv() => {
            info!(target: "server", "{} forcibly disconnected by heartbeat timeout", peer);
            Ok(())
        }
    };

    // Cleanup
    writer_task.abort();
    session_manager.force_remove_session(&session_keys.session_id).await;
    _connection_manager.unregister_connection(&session_keys.session_id).await;
    drop(timer);
    SecurityMetrics::active_connections().dec();

    info!(target: "server", "{} closed", peer);
    process_result
}

async fn write_task(writer: tokio::net::tcp::OwnedWriteHalf, mut out_rx: mpsc::Receiver<Vec<u8>>) {
    let mut batch: Vec<Vec<u8>> = Vec::new();
    let mut writer = writer;

    loop {
        tokio::select! {
            Some(resp) = out_rx.recv() => {
                batch.push(resp);
                if batch.len() >= 8 {
                    if flush_batch(&mut writer, &mut batch).await.is_err() {
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                if !batch.is_empty() {
                    if flush_batch(&mut writer, &mut batch).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
}

async fn flush_batch(writer: &mut tokio::net::tcp::OwnedWriteHalf, batch: &mut Vec<Vec<u8>>) -> anyhow::Result<()> {
    for pkt in batch.drain(..) {
        write_frame(writer, &pkt).await?;
    }
    Ok(())
}

async fn process_loop(
    mut reader: tokio::net::tcp::OwnedReadHalf,
    peer: SocketAddr,
    ctx: Arc<SessionKeys>,
    dispatcher: Arc<Dispatcher>,
    out_tx: mpsc::Sender<Vec<u8>>,
    session_manager: Arc<SessionManager>,
) -> anyhow::Result<()> {
    let mut total_bytes_received = 0;
    let start_time = Instant::now();
    let mut last_activity = Instant::now();

    loop {
        // Проверяем таймаут неактивности
        if last_activity.elapsed() > INACTIVITY_TIMEOUT {
            warn!(target: "server", "{} inactive for {:?}, closing connection", peer, last_activity.elapsed());
            break;
        }

        // Читаем фрейм с таймаутом
        match tokio::time::timeout(Duration::from_secs(5), read_frame(&mut reader)).await {
            Ok(Ok(frame)) => {
                last_activity = Instant::now(); // Обновляем время активности

                if !handle_frame(
                    &frame,
                    peer,
                    &ctx,
                    &dispatcher,
                    &out_tx,
                    &mut total_bytes_received,
                    start_time,
                    &session_manager,
                ).await {
                    break;
                }
            }
            Ok(Err(e)) => {
                info!(target: "server", "{} disconnected: {}", peer, e);
                break;
            }
            Err(_) => {
                // Таймаут чтения - продолжаем цикл для проверки активности
                continue;
            }
        }
    }

    Ok(())
}

// Обновим функцию handle_frame:
async fn handle_frame(
    frame: &[u8],
    peer: SocketAddr,
    ctx: &Arc<SessionKeys>,
    dispatcher: &Arc<Dispatcher>,
    out_tx: &mpsc::Sender<Vec<u8>>,
    total_bytes_received: &mut usize,
    start_time: Instant,
    session_manager: &Arc<SessionManager>,
) -> bool {
    let frame_start = Instant::now();
    let ip_str = peer.ip().to_string();

    // Rate limiting per packet с логированием
    let rate_limit_start = Instant::now();
    let allowed = RATE_LIMITER.check_packet(&ip_str, frame);
    let rate_limit_time = rate_limit_start.elapsed();

    if rate_limit_time > Duration::from_millis(1) {
        debug!("Rate limit check took {:?} for {}", rate_limit_time, peer);
    }

    let priority = crate::core::protocol::packets::processor::priority::determine_priority(frame);
    let _ = SecurityAudit::log_rate_limit_event(&ip_str, frame.len(), &format!("{:?}", priority), allowed).await;

    if !allowed {
        warn!("Rate limit exceeded for {} (size: {}, priority: {:?})", peer, frame.len(), priority);
        return false;
    }

    // Check packet size limit
    if frame.len() > MAX_PACKET_SIZE {
        warn!("Oversized packet from {}: {} bytes", peer, frame.len());
        send_error(ctx, out_tx, "packet too large").await;
        return false;
    }

    // Bandwidth limiting
    *total_bytes_received += frame.len();
    let elapsed = start_time.elapsed().as_secs_f64();
    if elapsed > 0.0 {
        let bandwidth = *total_bytes_received as f64 / elapsed;
        if bandwidth > 1024.0 * 1024.0 {
            warn!("Bandwidth limit exceeded for {}: {:.2} MB/s", peer, bandwidth / 1024.0 / 1024.0);
            return false;
        }
    }

    // Обработка heartbeat пакетов
    if frame.len() >= 1 && frame[0] == 0x10 {
        let heartbeat_start = Instant::now();
        info!(target: "heartbeat", "Heartbeat received from {} session: {}", peer, hex::encode(&ctx.session_id));

        // Обновляем heartbeat в менеджере
        session_manager.on_heartbeat_received(&ctx.session_id).await;

        // Отправляем ответ
        let response = crate::core::protocol::packets::encoder::packet_builder::PacketBuilder::build_encrypted_packet(
            ctx,
            0x10, // Heartbeat response
            b"pong",
        ).await;

        let send_start = Instant::now();
        let _ = out_tx.send(response).await;
        let send_time = send_start.elapsed();

        let total_heartbeat_time = heartbeat_start.elapsed();
        trace!("Heartbeat processing - total: {:?}, send: {:?}", total_heartbeat_time, send_time);

        return true;
    }

    // Process the frame
    let _process_start = Instant::now();
    process_valid_frame(frame, peer, ctx, dispatcher, out_tx).await;
    let total_frame_time = frame_start.elapsed();

    if total_frame_time > Duration::from_millis(10) {
        debug!("Frame processing took {:?} for {} (size: {} bytes)",
               total_frame_time, peer, frame.len());
    }

    true
}

// Обновим функцию process_valid_frame:
async fn process_valid_frame(
    frame: &[u8],
    peer: SocketAddr,
    ctx: &Arc<SessionKeys>,
    dispatcher: &Arc<Dispatcher>,
    out_tx: &mpsc::Sender<Vec<u8>>,
) {
    let process_start = Instant::now();
    let priority = crate::core::protocol::packets::processor::priority::determine_priority(frame);
    let is_large = frame.len() > LARGE_THRESHOLD;

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();

    let work = Work {
        ctx: Arc::clone(ctx),
        raw_payload: frame.to_vec(),
        client_ip: peer,
        reply: reply_tx,
        received_at: Instant::now(),
        priority,
        is_large,
    };

    let submit_start = Instant::now();
    if dispatcher.submit(work).await.is_err() {
        warn!("Dispatcher submit failed for {}", peer);
        send_error(ctx, out_tx, "server busy").await;
        return;
    }
    let submit_time = submit_start.elapsed();

    if submit_time > Duration::from_micros(100) {
        debug!("Dispatcher submit took {:?} for {}", submit_time, peer);
    }

    handle_response(reply_rx, ctx, out_tx).await;
    let total_process_time = process_start.elapsed();

    if total_process_time > Duration::from_millis(5) {
        info!("Total frame processing took {:?} for {} (size: {} bytes, priority: {:?})",
              total_process_time, peer, frame.len(), priority);
    }
}

// Обновим функцию handle_response:
async fn handle_response(
    reply_rx: tokio::sync::oneshot::Receiver<Vec<u8>>,
    ctx: &Arc<SessionKeys>,
    out_tx: &mpsc::Sender<Vec<u8>>,
) {
    let wait_start = Instant::now();
    match tokio::time::timeout(Duration::from_secs(10), reply_rx).await {
        Ok(Ok(resp_bytes)) => {
            let wait_time = wait_start.elapsed();
            let send_start = Instant::now();

            if let Err(e) = out_tx.send(resp_bytes).await {
                info!(target: "connection_manager", "Failed to send response: {}", e);
            }

            let send_time = send_start.elapsed();

            if wait_time > Duration::from_millis(5) {
                debug!("Response wait time: {:?}, send time: {:?}", wait_time, send_time);
            }
        }
        Ok(Err(_)) => {
            warn!("Handler channel closed");
            send_error(ctx, out_tx, "internal error").await;
        }
        Err(_) => {
            warn!("Handler timeout after {:?}", wait_start.elapsed());
            send_error(ctx, out_tx, "worker timeout").await;
        }
    }
}

async fn send_error(ctx: &Arc<SessionKeys>, out_tx: &mpsc::Sender<Vec<u8>>, message: &str) {
    let error_packet = crate::core::protocol::packets::encoder::packet_builder::PacketBuilder::build_encrypted_packet(
        ctx, 0xFF, message.as_bytes()
    ).await;

    let _ = out_tx.send(error_packet).await;
}