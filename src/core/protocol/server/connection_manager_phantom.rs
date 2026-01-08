use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{Instant, Duration};
use tracing::{info, warn, debug};

use crate::core::protocol::phantom_crypto::keys::PhantomSession;
use crate::core::protocol::crypto::crypto_pool_phantom::PhantomCryptoPool;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::server::security::rate_limiter::instance::RATE_LIMITER;
use crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;
use crate::core::protocol::packets::processor::packet_service::{PhantomPacketService};

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
    phantom_crypto_pool: Arc<PhantomCryptoPool>,
    phantom_session_manager: Arc<PhantomSessionManager>,
    connection_manager: Arc<PhantomConnectionManager>,
    heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    // Добавляем PhantomPacketService в параметры
    packet_service: Arc<PhantomPacketService>,
) -> anyhow::Result<()> {
    let session_id = session.session_id();
    info!(target: "server", "💓 Starting heartbeat-integrated phantom connection for session: {} from {}",
        hex::encode(session_id), peer);

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
    let (reader, writer) = stream.into_split();

    // Регистрируем соединение
    connection_manager.register_connection(
        session_id.to_vec(),
        shutdown_tx
    ).await;

    // Регистрируем сессию через heartbeat_manager
    debug!("💓 Registering session in heartbeat manager: {}", hex::encode(session_id));

    // Отправляем начальный алерт о подключении
    heartbeat_manager.send_custom_alert(
        crate::core::monitoring::unified_monitor::AlertLevel::Info,
        "phantom_connection",
        &format!("New phantom connection from {} with session {}",
                 peer, hex::encode(session_id))
    ).await;

    // Запускаем writer task с поддержкой heartbeat
    let writer_task = tokio::spawn(phantom_write_task(
        writer,
        heartbeat_manager.clone(),
        session_id.to_vec(),
        peer,
    ));

    // Основной цикл обработки с поддержкой принудительного закрытия
    let process_result = tokio::select! {
        result = phantom_process_loop(
            reader,
            peer,
            session.clone(),
            phantom_crypto_pool,
            phantom_session_manager.clone(),
            heartbeat_manager.clone(),
            packet_service.clone(), // Передаем packet_service
        ) => {
            result
        }
        _ = shutdown_rx.recv() => {
            info!(target: "server", "👻 {} forcibly disconnected by timeout", peer);
            Ok(())
        }
    };

    // Отправляем алерт о завершении соединения
    heartbeat_manager.send_custom_alert(
        crate::core::monitoring::unified_monitor::AlertLevel::Info,
        "phantom_connection",
        &format!("Phantom connection closed from {} with session {}",
                 peer, hex::encode(session_id))
    ).await;

    // Очистка
    writer_task.abort();
    phantom_session_manager.force_remove_session(session_id).await;
    connection_manager.unregister_connection(session_id).await;

    info!(target: "server", "👻 Phantom connection {} closed (session: {})",
        peer, hex::encode(session_id));

    // Проверяем здоровье heartbeat системы
    if let Err(e) = check_heartbeat_health(&heartbeat_manager).await {
        warn!("💓 Heartbeat health check failed on connection close: {}", e);
    }

    process_result
}

async fn phantom_write_task(
    writer: tokio::net::tcp::OwnedWriteHalf,
    heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    session_id: Vec<u8>,
    _peer: SocketAddr,
) {
    let writer = writer;
    let mut last_heartbeat_sent = Instant::now();
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

    loop {
        match writer.writable().await {
            Ok(()) => {
                // Проверяем, нужно ли отправить heartbeat
                if last_heartbeat_sent.elapsed() >= HEARTBEAT_INTERVAL {
                    // Отправляем heartbeat
                    if let Err(e) = send_heartbeat(&writer, &session_id).await {
                        warn!("💓 Failed to send heartbeat to session {}: {}",
                            hex::encode(&session_id), e);
                    } else {
                        debug!("💓 Heartbeat sent to session {}", hex::encode(&session_id));
                        last_heartbeat_sent = Instant::now();

                        // Регистрируем отправку heartbeat
                        heartbeat_manager.heartbeat_received(session_id.clone());
                    }
                }

                // Здесь будет логика отправки данных
                // Пока просто держим соединение
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
    // Создаем простый heartbeat пакет (0x10 - тип heartbeat)
    let heartbeat_packet = vec![0x10];

    // В реальной реализации здесь должна быть криптография
    match writer.try_write(&heartbeat_packet) {
        Ok(_) => {
            debug!("💓 Heartbeat packet sent for session {}", hex::encode(session_id));
            Ok(())
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                // Попробуем в следующий раз
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
    crypto_pool: Arc<PhantomCryptoPool>,
    session_manager: Arc<PhantomSessionManager>,
    heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    packet_service: Arc<PhantomPacketService>, // Добавляем packet_service
) -> anyhow::Result<()> {
    let mut last_activity = Instant::now();
    let session_id = session.session_id().to_vec();

    loop {
        // Проверяем таймаут неактивности
        if last_activity.elapsed() > INACTIVITY_TIMEOUT {
            warn!(target: "server", "👻 {} inactive for {:?}, closing connection",
                peer, last_activity.elapsed());

            // Отправляем алерт о таймауте
            heartbeat_manager.send_custom_alert(
                crate::core::monitoring::unified_monitor::AlertLevel::Warning,
                "phantom_connection",
                &format!("Inactivity timeout for session {} from {}",
                         hex::encode(&session_id), peer)
            ).await;

            break;
        }

        // Читаем данные с таймаутом
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

                        // Обновляем активность в heartbeat manager
                        heartbeat_manager.heartbeat_received(session_id.clone());

                        // Обрабатываем пакет через фантомный криптопул
                        if let Err(e) = handle_phantom_packet(
                            &buffer,
                            peer,
                            &session,
                            &crypto_pool,
                            &session_manager,
                            &heartbeat_manager,
                            &packet_service, // Передаем packet_service
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
                // Таймаут чтения - продолжаем цикл
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
    crypto_pool: &Arc<PhantomCryptoPool>,
    session_manager: &Arc<PhantomSessionManager>,
    heartbeat_manager: &Arc<ConnectionHeartbeatManager>,
    packet_service: &Arc<PhantomPacketService>, // Добавляем packet_service
) -> anyhow::Result<()> {
    let start = Instant::now();

    let ip_str = peer.ip().to_string();
    let session_id = session.session_id();

    // Rate limiting
    if !RATE_LIMITER.check_packet(&ip_str, data) {
        warn!("👻 Rate limit exceeded for phantom connection {}", peer);

        // Отправляем алерт о превышении rate limit
        heartbeat_manager.send_custom_alert(
            crate::core::monitoring::unified_monitor::AlertLevel::Warning,
            "rate_limit",
            &format!("Rate limit exceeded for session {} from {}",
                     hex::encode(session_id), peer)
        ).await;

        return Ok(());
    }

    // Проверка размера пакета
    if data.len() > MAX_PACKET_SIZE {
        warn!("👻 Oversized phantom packet from {}: {} bytes", peer, data.len());
        return Ok(());
    }

    // Обработка heartbeat пакетов (0x10)
    if data.len() >= 1 && data[0] == 0x10 {
        debug!(target: "phantom_heartbeat",
            "👻 Heartbeat received from {} session: {}",
            peer, hex::encode(session_id));

        // Обновляем активность сессии
        session_manager.on_heartbeat_received(session_id).await;

        // Регистрируем получение heartbeat
        heartbeat_manager.heartbeat_received(session_id.to_vec());

        debug!("💓 Heartbeat processed for session {}", hex::encode(session_id));
        return Ok(());
    }

    // Декодируем и обрабатываем пакет через фантомный криптопул
    match crypto_pool.decrypt(session.clone(), data.to_vec()).await {
        Ok((packet_type, plaintext)) => {
            let _elapsed = start.elapsed();

            // Замер времени обработки пакета
            #[cfg(feature = "metrics")]
            metrics::histogram!("phantom.connection.packet_process_time", _elapsed.as_micros() as f64);

            debug!(
                "👻 Successfully decrypted phantom packet from {}: type=0x{:02X}, size={} bytes",
                peer, packet_type, plaintext.len()
            );

            // Обработка расшифрованных данных через PhantomPacketService
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
            warn!("👻 Failed to decrypt phantom packet from {}: {}", peer, e);

            // Отправляем алерт о проблеме с дешифрованием
            heartbeat_manager.send_custom_alert(
                crate::core::monitoring::unified_monitor::AlertLevel::Warning,
                "decryption_error",
                &format!("Decryption failed for session {} from {}: {}",
                         hex::encode(session_id), peer, e)
            ).await;
        }
    }

    Ok(())
}

async fn process_decrypted_phantom_payload(
    packet_type: u8,
    plaintext: Vec<u8>, // Принимаем владение данными
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

    // Вся бизнес-логика теперь вынесена в PhantomPacketService
    match packet_service.process_packet(
        session.clone(),
        packet_type,
        plaintext,
        peer,
    ).await {
        Ok(processing_result) => {
            // Здесь можно обработать результат, если нужно отправить ответ
            debug!("👻 Packet processing result: should_encrypt={}, response_size={} bytes",
                   processing_result.should_encrypt, processing_result.response.len());

            // TODO: Добавить логику отправки ответа клиенту, если необходимо
            // Это может потребовать доступ к writer или отдельный канал для ответов
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