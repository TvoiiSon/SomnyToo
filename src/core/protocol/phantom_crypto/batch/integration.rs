use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, error, debug, warn};

use crate::core::monitoring::unified_monitor::UnifiedMonitor;
use crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::packets::packet_service::PhantomPacketService;

use crate::core::protocol::phantom_crypto::batch::config::BatchConfig;
use crate::core::protocol::phantom_crypto::batch::core::reader::{BatchReader, ReaderEvent};
use crate::core::protocol::phantom_crypto::batch::core::writer::BatchWriter;
use crate::core::protocol::phantom_crypto::batch::core::dispatcher::PacketDispatcher;
use crate::core::protocol::phantom_crypto::batch::types::error::BatchError;
use crate::core::protocol::phantom_crypto::batch::types::priority::Priority;

/// Интегрированная batch система
pub struct BatchSystem {
    config: BatchConfig,
    reader: Arc<BatchReader>,
    writer: Arc<BatchWriter>,
    dispatcher: Arc<PacketDispatcher>,
    packet_service: Arc<PhantomPacketService>,

    // Каналы событий
    reader_events_tx: mpsc::Sender<ReaderEvent>,
    reader_events_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<ReaderEvent>>>,

    // Мониторинг и управление
    monitor: Arc<UnifiedMonitor>,
    session_manager: Arc<PhantomSessionManager>,
    crypto: Arc<PhantomCrypto>,

    is_running: Arc<std::sync::atomic::AtomicBool>,
}

impl BatchSystem {
    pub async fn new(
        config: BatchConfig,
        monitor: Arc<UnifiedMonitor>,
        session_manager: Arc<PhantomSessionManager>,
        crypto: Arc<PhantomCrypto>,
    ) -> Result<Self, BatchError> {
        info!("🚀 Initializing Batch System...");

        // Создаем каналы для событий
        let (reader_events_tx, reader_events_rx) = mpsc::channel(1000);

        // Создаем packet service
        let packet_service = Arc::new(PhantomPacketService::new(
            session_manager.clone(),
            {
                use crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;
                Arc::new(ConnectionHeartbeatManager::new(
                    session_manager.clone(),
                    monitor.clone(),
                ))
            },
        ));

        // Создаем компоненты
        let reader = Arc::new(BatchReader::new(config.clone(), reader_events_tx.clone()));
        let writer = Arc::new(BatchWriter::new(config.clone()));

        let dispatcher = Arc::new(PacketDispatcher::new(
            config.clone(),
            session_manager.clone(),
            packet_service.clone(),
            writer.clone(),
        ).await);

        let system = Self {
            config,
            reader,
            writer,
            dispatcher,
            packet_service,
            reader_events_tx,
            reader_events_rx: Arc::new(tokio::sync::Mutex::new(reader_events_rx)),
            monitor: monitor.clone(),
            session_manager: session_manager.clone(),
            crypto: crypto.clone(),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };

        // Запускаем обработчик событий
        system.start_event_handler().await;

        // Запускаем мониторинг канала (теперь используем reader_events_tx)
        system.start_channel_monitoring().await;

        info!("✅ Batch System initialized successfully");

        Ok(system)
    }

    async fn start_event_handler(&self) {
        let dispatcher = self.dispatcher.clone();
        let is_running = self.is_running.clone();

        let reader_events_rx = self.reader_events_rx.clone();

        tokio::spawn(async move {
            let mut rx = reader_events_rx.lock().await;

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                match rx.recv().await {
                    Some(event) => {
                        match event {
                            ReaderEvent::DataReady {
                                session_id,
                                data,
                                source_addr,
                                priority,
                                received_at
                            } => {
                                info!("📨 Event received from {}: {} bytes", source_addr, data.len());
                                let task = crate::core::protocol::phantom_crypto::batch::core::dispatcher::DispatchTask {
                                    session_id,
                                    data,
                                    source_addr,
                                    priority,
                                    received_at,
                                };

                                info!("📤 Submitting task to dispatcher...");
                                match dispatcher.submit_task(task).await {
                                    Ok(_) => {
                                        info!("✅ Event submitted to dispatcher");
                                    }
                                    Err(e) => {
                                        error!("❌ Failed to submit task to dispatcher: {}", e);
                                    }
                                }
                            }
                            ReaderEvent::ConnectionClosed { source_addr, reason } => {
                                debug!("Connection closed: {} - {}", source_addr, reason);
                            }
                            ReaderEvent::Error { source_addr, error } => {
                                error!("Reader error from {}: {}", source_addr, error);
                            }
                        }
                    }
                    None => {
                        // Канал закрыт
                        debug!("Channel closed, stopping event handler");
                        break;
                    }
                }
            }
        });
    }

    // ДОБАВЛЯЕМ метод для мониторинга состояния канала
    async fn start_channel_monitoring(&self) {
        let reader_events_tx = self.reader_events_tx.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut check_count = 0;

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                check_count += 1;

                // Проверяем состояние канала
                let is_closed = reader_events_tx.is_closed();
                let capacity = reader_events_tx.capacity();

                debug!("Channel monitoring check #{}: closed={}, capacity={}",
                    check_count, is_closed, capacity);

                // Если канал закрыт, логируем ошибку
                if is_closed {
                    error!("Reader events channel is closed!");
                }

                // Если емкость канала мала, логируем предупреждение
                if capacity < 100 {
                    warn!("Reader events channel running low on capacity: {}", capacity);
                }
            }
        });
    }

    // Методы для работы с соединениями

    pub async fn register_connection(
        &self,
        source_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        read_stream: Box<dyn tokio::io::AsyncRead + Unpin + Send + Sync>,
        write_stream: Box<dyn tokio::io::AsyncWrite + Unpin + Send + Sync>,
    ) -> Result<(), BatchError> {
        self.reader.register_connection(
            source_addr,
            session_id.clone(),
            read_stream,
        ).await?;

        self.writer.register_connection(
            source_addr,
            session_id,
            write_stream,
        ).await?;

        Ok(())
    }

    pub async fn write(
        &self,
        destination_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        data: bytes::Bytes,
        priority: Priority,
        requires_flush: bool,
    ) -> Result<(), BatchError> {
        self.writer.write(
            destination_addr,
            session_id,
            data,
            priority,
            requires_flush,
        ).await
    }

    // Вспомогательные методы

    pub async fn send_pong_response(
        &self,
        destination_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
    ) -> Result<(), BatchError> {
        self.write(
            destination_addr,
            session_id,
            bytes::Bytes::from_static(b"\x02PONG"),
            Priority::Critical,
            true,
        ).await
    }

    pub async fn send_heartbeat_response(
        &self,
        destination_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
    ) -> Result<(), BatchError> {
        self.write(
            destination_addr,
            session_id,
            bytes::Bytes::from_static(b"\x10Heartbeat acknowledged"),
            Priority::Critical,
            true,
        ).await
    }

    // ДОБАВЛЯЕМ метод для получения статуса канала
    pub fn get_channel_status(&self) -> ChannelStatus {
        let is_closed = self.reader_events_tx.is_closed();
        let capacity = self.reader_events_tx.capacity();

        ChannelStatus {
            is_closed,
            capacity,
            is_healthy: !is_closed && capacity > 100,
        }
    }

    // Получение компонентов

    pub fn reader(&self) -> Arc<BatchReader> {
        self.reader.clone()
    }

    pub fn writer(&self) -> Arc<BatchWriter> {
        self.writer.clone()
    }

    pub fn dispatcher(&self) -> Arc<PacketDispatcher> {
        self.dispatcher.clone()
    }

    pub fn packet_service(&self) -> Arc<PhantomPacketService> {
        self.packet_service.clone()
    }

    pub fn session_manager(&self) -> Arc<PhantomSessionManager> {
        self.session_manager.clone()
    }

    // Управление системой

    pub async fn shutdown(&self) {
        info!("Shutting down Batch System...");

        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);

        self.reader.shutdown().await;
        self.writer.shutdown().await;
        self.dispatcher.shutdown().await;

        // Логируем статус канала перед завершением
        let status = self.get_channel_status();
        info!("Final channel status: closed={}, capacity={}, healthy={}",
            status.is_closed, status.capacity, status.is_healthy);

        info!("Batch System shutdown complete");
    }
}

/// ДОБАВЛЯЕМ структуру для статуса канала
#[derive(Debug, Clone)]
pub struct ChannelStatus {
    pub is_closed: bool,
    pub capacity: usize,
    pub is_healthy: bool,
}

impl ChannelStatus {
    pub fn to_string(&self) -> String {
        format!(
            "Closed: {}, Capacity: {}, Healthy: {}",
            self.is_closed, self.capacity, self.is_healthy
        )
    }
}

impl Clone for BatchSystem {
    fn clone(&self) -> Self {
        let (reader_events_tx, reader_events_rx) = mpsc::channel(1000);

        Self {
            config: self.config.clone(),
            reader: Arc::new(BatchReader::new(
                self.config.clone(),
                reader_events_tx.clone(),
            )),
            writer: Arc::new(BatchWriter::new(self.config.clone())),
            dispatcher: self.dispatcher.clone(),
            packet_service: self.packet_service.clone(),
            reader_events_tx,
            reader_events_rx: Arc::new(tokio::sync::Mutex::new(reader_events_rx)),
            monitor: self.monitor.clone(),
            session_manager: self.session_manager.clone(),
            crypto: self.crypto.clone(),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
}