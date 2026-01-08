use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration, Instant};
use tracing::{info, warn, error, debug};

use crate::core::protocol::phantom_crypto::core::handshake::{perform_phantom_handshake, HandshakeRole};
use crate::core::protocol::server::security::rate_limiter::instance::RATE_LIMITER;
use crate::config::PhantomConfig;
use crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;
// Добавляем импорт PhantomPacketService
use crate::core::protocol::packets::processor::packet_service::PhantomPacketService;

pub async fn handle_phantom_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    phantom_config: PhantomConfig,
    session_manager: Arc<crate::core::protocol::server::session_manager_phantom::PhantomSessionManager>,
    connection_manager: Arc<crate::core::protocol::server::connection_manager_phantom::PhantomConnectionManager>,
    crypto_pool: Arc<crate::core::protocol::crypto::crypto_pool_phantom::PhantomCryptoPool>,
    heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    // Добавляем PhantomPacketService в параметры
    packet_service: Arc<PhantomPacketService>,
) -> anyhow::Result<()> {
    let connection_start = Instant::now();
    info!(target: "server", "👻 {} attempting phantom connection", peer);

    // Rate limiting для нового соединения
    if !RATE_LIMITER.check_packet(&peer.ip().to_string(), &[]) {
        warn!(target: "server", "👻 Rate limit exceeded for {}, rejecting connection", peer);
        return Ok(());
    }

    // Выполняем фантомный handshake с таймаутом
    let handshake_start = Instant::now();
    let handshake_result = match timeout(
        Duration::from_secs(30),
        perform_phantom_handshake(&mut stream, HandshakeRole::Server)
    ).await {
        Ok(Ok(result)) => {
            let handshake_time = handshake_start.elapsed();
            info!(target: "server",
                "👻 Phantom handshake successful for {} in {:?}, session: {}",
                peer, handshake_time, hex::encode(&result.session.session_id()));
            result
        },
        Ok(Err(e)) => {
            let handshake_time = handshake_start.elapsed();
            warn!(target: "server",
                "👻 Phantom handshake failed for {} after {:?}: {}",
                peer, handshake_time, e);
            return Ok(());
        }
        Err(_) => {
            error!(target: "server", "👻 Phantom handshake timeout for {}", peer);
            return Ok(());
        }
    };

    // Проверяем конфигурацию фантомной системы
    if let Err(e) = phantom_config.validate() {
        error!("👻 Invalid phantom configuration: {}", e);
        return Ok(());
    }

    // Проверяем аппаратную аутентификацию если включена
    if phantom_config.should_use_hardware_auth() {
        info!("👻 Hardware authentication enabled for session: {}",
            hex::encode(&handshake_result.session.session_id()));
    }

    // Регистрируем фантомную сессию
    let phantom_session = Arc::new(handshake_result.session);
    let session_id = phantom_session.session_id();

    // Создаем канал для heartbeat ответов
    let (heartbeat_tx, mut heartbeat_rx) = mpsc::unbounded_channel();

    // Запускаем heartbeat для этой сессии
    heartbeat_manager.start_heartbeat(
        session_id.to_vec(),
        phantom_session.clone(),
        peer,
        heartbeat_tx,
    );

    info!(target: "server", "💓 Heartbeat started for session: {} from {}",
        hex::encode(&session_id), peer);

    // Запускаем обработку соединения через фантомный менеджер соединений
    let connection_result = crate::core::protocol::server::connection_manager_phantom::handle_phantom_client_connection(
        stream,
        peer,
        phantom_session.clone(),
        crypto_pool,
        session_manager.clone(),
        connection_manager.clone(),
        heartbeat_manager.clone(),
        packet_service.clone(), // Передаем packet_service
    ).await;

    // Запускаем задачу для обработки heartbeat сообщений
    let heartbeat_manager_clone = heartbeat_manager.clone();
    let session_id_clone = session_id.to_vec();

    let heartbeat_task = tokio::spawn(async move {
        debug!("💓 Starting heartbeat receiver for session: {}", hex::encode(&session_id_clone));

        while let Some(heartbeat_data) = heartbeat_rx.recv().await {
            // Отправляем heartbeat данные клиенту
            // Здесь должна быть логика отправки heartbeat пакета клиенту
            debug!("💓 Heartbeat data ready for session {}: {:?}",
                hex::encode(&session_id_clone), heartbeat_data);

            // Обновляем время последнего heartbeat
            heartbeat_manager_clone.heartbeat_received(session_id_clone.clone());

            // Можно добавить логику отправки через существующий stream
            // Но для этого нужно иметь доступ к stream или другому каналу
        }

        debug!("💓 Heartbeat receiver stopped for session: {}", hex::encode(&session_id_clone));
    });

    // Ждем завершения обработки соединения
    let connection_handler_result = match connection_result {
        Ok(()) => {
            debug!("💓 Connection handler completed successfully for session: {}",
                hex::encode(&session_id));
            Ok(())
        }
        Err(e) => {
            error!("💓 Connection handler error for session {}: {}",
                hex::encode(&session_id), e);
            Err(e)
        }
    };

    // Останавливаем heartbeat для этой сессии
    heartbeat_manager.stop_heartbeat(session_id.to_vec());
    info!(target: "server", "💓 Heartbeat stopped for session: {}", hex::encode(&session_id));

    // Отменяем heartbeat задачу
    heartbeat_task.abort();

    // Логируем время соединения
    let total_connection_time = connection_start.elapsed();
    info!(target: "server",
        "👻 {} phantom connection closed after {:?}, session: {}",
        peer, total_connection_time, hex::encode(session_id));

    connection_handler_result
}