use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, debug, warn, error};

use crate::core::monitoring::unified_monitor::UnifiedMonitor;
use crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;

// Импортируем batch компоненты через правильные пути
use crate::core::protocol::phantom_crypto::batch::{
    io::reader::batch_reader::{BatchReader, BatchReaderConfig, BatchReaderEvent, BatchFrame},
    io::writer::batch_writer::{BatchWriter, BatchWriterConfig, BatchWriterEvent, WritePriority},
    processor::crypto_batch_processor::{CryptoBatchProcessor, BatchCryptoConfig},
    dispatcher::packet_batch_dispatcher::{PacketBatchDispatcher, PacketBatchDispatcherConfig},
    buffer::unified_buffer_pool::{UnifiedBufferPool, BufferPoolConfig},
    types::error::BatchError,
};

use crate::core::protocol::phantom_crypto::batch::dispatcher::task::DispatchTask;
// УБРАЛИ неиспользуемый импорт: use crate::core::protocol::phantom_crypto::batch::types::priority::BatchPriority;

// Импортируем packet service для обработки пакетов
use crate::core::protocol::packets::packet_service::PhantomPacketService;
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;

/// Интеграционная структура, объединяющая все batch компоненты
pub struct PhantomBatchSystem {
    pub batch_reader: Arc<BatchReader>,
    pub batch_writer: Arc<BatchWriter>,
    pub crypto_batch_processor: Arc<CryptoBatchProcessor>,
    pub packet_dispatcher: Arc<PacketBatchDispatcher>,
    pub buffer_pool: Arc<UnifiedBufferPool>,
    pub packet_service: Arc<PhantomPacketService>,

    // Каналы для коммуникации между компонентами
    reader_events_tx: mpsc::Sender<BatchReaderEvent>,
    writer_events_tx: mpsc::Sender<BatchWriterEvent>,

    // Сохраняем ссылки для использования в методах
    monitor: Arc<UnifiedMonitor>,
    session_manager: Arc<PhantomSessionManager>,
    crypto: Arc<PhantomCrypto>,
}

impl PhantomBatchSystem {
    pub async fn new(
        monitor: Arc<UnifiedMonitor>,
        session_manager: Arc<PhantomSessionManager>,
        crypto_pool: Arc<PhantomCrypto>,
    ) -> Self {
        info!("🚀 Initializing Phantom Batch System...");

        // Сохраняем ссылки для использования
        let monitor_clone = monitor.clone();
        let session_manager_clone = session_manager.clone();
        let crypto_clone = crypto_pool.clone();

        // Конфигурации
        let buffer_pool_config = BufferPoolConfig::default();
        let reader_config = BatchReaderConfig::default();
        let writer_config = BatchWriterConfig::default();
        let crypto_config = BatchCryptoConfig::default();
        let dispatcher_config = PacketBatchDispatcherConfig::default();

        // Создаем каналы для событий
        let (reader_events_tx, reader_events_rx) = mpsc::channel(1000);
        let (writer_events_tx, writer_events_rx) = mpsc::channel(1000);

        // Создаем пул буферов
        let buffer_pool = Arc::new(UnifiedBufferPool::new(buffer_pool_config));

        // Создаем packet service для обработки пакетов
        let packet_service = Arc::new(PhantomPacketService::new(
            session_manager_clone.clone(),
            // Создаем heartbeat manager для packet service
            {
                use crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;
                Arc::new(ConnectionHeartbeatManager::new(
                    session_manager_clone.clone(),
                    monitor_clone.clone(),
                ))
            },
        ));

        // Создаем batch reader
        let batch_reader = Arc::new(BatchReader::new(reader_config, reader_events_tx.clone()));

        // Создаем batch writer
        let batch_writer = Arc::new(BatchWriter::new(writer_config, writer_events_tx.clone()));

        // Создаем crypto batch processor
        let crypto_batch_processor = Arc::new(CryptoBatchProcessor::new(crypto_config));

        // Создаем packet dispatcher - ВАЖНО: используем .await для получения результата Future
        let packet_dispatcher_future = PacketBatchDispatcher::new(
            dispatcher_config,
            crypto_batch_processor.clone(),
            batch_writer.clone(),
            monitor_clone.clone(),
        );
        let packet_dispatcher = Arc::new(packet_dispatcher_future.await);

        // Запускаем обработчики событий
        Self::start_event_handlers(
            reader_events_rx,
            writer_events_rx,
            packet_dispatcher.clone(),
            packet_service.clone(),
            batch_writer.clone(),
            monitor_clone.clone(),
            session_manager_clone.clone(),
            crypto_clone.clone(),
        );

        info!("✅ Phantom Batch System initialized successfully");

        Self {
            batch_reader,
            batch_writer,
            crypto_batch_processor,
            packet_dispatcher,
            buffer_pool,
            packet_service,
            reader_events_tx,
            writer_events_tx,
            monitor: monitor_clone,
            session_manager: session_manager_clone,
            crypto: crypto_clone,
        }
    }

    /// Основной метод обработки входящих фреймов
    async fn process_incoming_frame(
        frame: &BatchFrame,
        source_addr: std::net::SocketAddr,
        packet_service: &Arc<PhantomPacketService>,
        session_manager: &Arc<PhantomSessionManager>,
        batch_writer: &Arc<BatchWriter>,
    ) {
        if frame.data.is_empty() {
            debug!("Empty frame from {}", source_addr);
            return;
        }

        // Получаем сессию
        match session_manager.get_session(&frame.session_id).await {
            Some(session) => {
                // Используем PhantomPacketProcessor для расшифровки
                let packet_processor = PhantomPacketProcessor::new();

                match packet_processor.process_incoming_vec(&frame.data, &session) {
                    Ok((actual_packet_type, decrypted_payload)) => {
                        debug!("Decrypted packet type 0x{:02x} from {} session: {}",
                       actual_packet_type, source_addr, hex::encode(&frame.session_id));

                        // Обрабатываем через packet service с РАСШИФРОВАННЫМИ данными
                        match packet_service.process_packet(
                            session.clone(),
                            actual_packet_type,
                            decrypted_payload,
                            source_addr,
                        ).await {
                            Ok(processing_result) => {
                                // Теперь нужно ЗАШИФРОВАТЬ ответ!
                                match packet_processor.create_outgoing_vec(
                                    &session,
                                    processing_result.packet_type,
                                    &processing_result.response
                                ) {
                                    Ok(encrypted_response) => {
                                        // Отправляем зашифрованный ответ
                                        let priority = match processing_result.packet_type {
                                            0x02 => WritePriority::Immediate, // PONG
                                            0x10 => WritePriority::Immediate, // Heartbeat
                                            _ => WritePriority::Normal,
                                        };

                                        if let Err(e) = batch_writer.queue_write(
                                            source_addr,
                                            frame.session_id.clone(),
                                            bytes::Bytes::from(encrypted_response),
                                            priority,
                                            true,
                                        ).await {
                                            warn!("Failed to send encrypted response to {}: {}", source_addr, e);
                                        } else {
                                            debug!("Encrypted response sent to {}", source_addr);
                                        }
                                    }
                                    Err(e) => {
                                        error!("Failed to encrypt response for {}: {}", source_addr, e);
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Failed to process decrypted packet from {}: {}", source_addr, e);
                                // Отправляем зашифрованную ошибку
                                let error_msg = format!("Error: {}", e);
                                match packet_processor.create_outgoing_vec(
                                    &session,
                                    0xFF, // Error packet type
                                    error_msg.as_bytes()
                                ) {
                                    Ok(encrypted_error) => {
                                        let _ = batch_writer.queue_write(
                                            source_addr,
                                            frame.session_id.clone(),
                                            bytes::Bytes::from(encrypted_error),
                                            WritePriority::Immediate,
                                            true,
                                        ).await;
                                    }
                                    Err(encrypt_err) => {
                                        error!("Failed to encrypt error for {}: {}", source_addr, encrypt_err);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to decrypt phantom packet from {}: {}", source_addr, e);
                        // Не можем отправить зашифрованный ответ без успешной расшифровки
                        // Возможно, отправляем простой текст или просто закрываем соединение
                        warn!("Decryption failed for {}, possible session mismatch", source_addr);
                    }
                }
            }
            None => {
                warn!("Session not found for frame from {}: session_id: {}",
              source_addr, hex::encode(&frame.session_id));
                // Нет сессии - не можем расшифровать
            }
        }
    }

    fn start_event_handlers(
        mut reader_events_rx: mpsc::Receiver<BatchReaderEvent>,
        mut writer_events_rx: mpsc::Receiver<BatchWriterEvent>,
        packet_dispatcher: Arc<PacketBatchDispatcher>,
        packet_service: Arc<PhantomPacketService>,
        batch_writer: Arc<BatchWriter>,
        _monitor: Arc<UnifiedMonitor>,
        session_manager: Arc<PhantomSessionManager>,
        _crypto: Arc<PhantomCrypto>,
    ) {
        let session_manager_clone = session_manager.clone();
        let batch_writer_clone = batch_writer.clone();
        let packet_service_clone = packet_service.clone();

        // Обработчик событий от reader
        tokio::spawn(async move {
            info!("📊 Batch reader event handler started");

            while let Some(event) = reader_events_rx.recv().await {
                match event {
                    BatchReaderEvent::BatchReady { batch_id, frames, source_addr, received_at } => {
                        debug!("Processing batch #{} from {} with {} frames",
                           batch_id, source_addr, frames.len());

                        // Обрабатываем каждый фрейм через packet service
                        for frame in &frames {
                            // Асинхронная обработка каждого фрейма
                            let frame_clone = frame.clone();
                            let packet_service_local = packet_service_clone.clone();
                            let session_manager_local = session_manager_clone.clone();
                            let batch_writer_local = batch_writer_clone.clone();
                            let source_addr_local = source_addr;

                            tokio::spawn(async move {
                                Self::process_incoming_frame(
                                    &frame_clone,
                                    source_addr_local,
                                    &packet_service_local,
                                    &session_manager_local,
                                    &batch_writer_local,
                                ).await;
                            });
                        }

                        // Также передаем batch в диспетчер для batch обработки
                        packet_dispatcher.process_batch_from_reader(
                            BatchReaderEvent::BatchReady {
                                batch_id,
                                frames,
                                source_addr,
                                received_at
                            }
                        ).await;
                    }
                    BatchReaderEvent::ConnectionClosed { ref source_addr, ref reason } => {
                        info!("Connection closed: {} - {}", source_addr, reason);

                        // Уведомляем packet dispatcher с клонированными значениями
                        packet_dispatcher.process_batch_from_reader(
                            BatchReaderEvent::ConnectionClosed {
                                source_addr: *source_addr,
                                reason: reason.clone()
                            }
                        ).await;

                        debug!("Connection {} closed: {}", source_addr, reason);
                    }
                    BatchReaderEvent::ReadError { ref source_addr, ref error } => {
                        error!("Read error from {}: {}", source_addr, error);

                        // Уведомляем packet dispatcher с клонированными значениями
                        packet_dispatcher.process_batch_from_reader(
                            BatchReaderEvent::ReadError {
                                source_addr: *source_addr,
                                error: error.clone()
                            }
                        ).await;

                        error!("Read error from {}: {}", source_addr, error);
                    }
                    BatchReaderEvent::StatisticsUpdate { stats } => {
                        // Логируем статистику
                        if stats.total_frames_read % 100 == 0 {
                            info!("Reader stats: {} fps, {} bytes/s, {} total frames",
                            stats.frames_per_second,
                            stats.bytes_per_second,
                            stats.total_frames_read);
                        }
                    }
                }
            }
        });

        // Обработчик событий от writer
        tokio::spawn(async move {
            info!("📊 Batch writer event handler started");

            while let Some(event) = writer_events_rx.recv().await {
                match event {
                    BatchWriterEvent::WriteCompleted {
                        destination_addr,
                        batch_id,
                        bytes_written,
                        write_time
                    } => {
                        debug!("Write completed for {}: batch #{}, {} bytes in {:?}",
                            destination_addr, batch_id, bytes_written, write_time);
                    }
                    BatchWriterEvent::WriteError { destination_addr, error } => {
                        error!("Write error for {}: {}", destination_addr, error);
                    }
                    BatchWriterEvent::BufferFull { destination_addr, buffer_size } => {
                        warn!("Buffer full for {}: {} bytes", destination_addr, buffer_size);
                    }
                    BatchWriterEvent::StatisticsUpdate { stats } => {
                        // Логируем статистику
                        if stats.total_writes % 100 == 0 {
                            debug!("Writer stats: {} wps, {} bytes/s, {} total writes",
                                stats.writes_per_second,
                                stats.bytes_per_second,
                                stats.total_writes);
                        }
                    }
                }
            }
        });
    }

    pub async fn submit_to_dispatcher(&self, task: DispatchTask) -> Result<(), BatchError> {
        self.packet_dispatcher.submit_task(task).await
    }

    pub async fn cleanup_buffers(&self, max_age: Duration) {
        self.buffer_pool.cleanup_old_buffers(max_age);
    }

    pub fn log_buffer_stats(&self) {
        self.buffer_pool.log_pool_stats();
    }

    // Добавляем методы для использования каналов
    pub async fn send_reader_event(&self, event: BatchReaderEvent) -> Result<(), mpsc::error::SendError<BatchReaderEvent>> {
        self.reader_events_tx.send(event).await
    }

    pub async fn send_writer_event(&self, event: BatchWriterEvent) -> Result<(), mpsc::error::SendError<BatchWriterEvent>> {
        self.writer_events_tx.send(event).await
    }

    // Метод для получения монитора
    pub fn monitor(&self) -> Arc<UnifiedMonitor> {
        self.monitor.clone()
    }

    // Метод для получения session manager
    pub fn session_manager(&self) -> Arc<PhantomSessionManager> {
        self.session_manager.clone()
    }

    // Метод для получения crypto
    pub fn crypto(&self) -> Arc<PhantomCrypto> {
        self.crypto.clone()
    }

    // Метод для получения packet service
    pub fn packet_service(&self) -> Arc<PhantomPacketService> {
        self.packet_service.clone()
    }

    // Метод для отправки PING ответа (PONG)
    pub async fn send_pong_response(&self, destination_addr: std::net::SocketAddr, session_id: Vec<u8>) -> Result<(), BatchError> {
        let pong_data = bytes::Bytes::from_static(b"\x02PONG"); // 0x02 = PONG packet type

        self.batch_writer.queue_write(
            destination_addr,
            session_id,
            pong_data,
            WritePriority::Immediate,
            true,
        ).await
            .map_err(|e| BatchError::ProcessingFailed(e.to_string()))
    }

    // Метод для отправки heartbeat ответа
    pub async fn send_heartbeat_response(&self, destination_addr: std::net::SocketAddr, session_id: Vec<u8>) -> Result<(), BatchError> {
        let heartbeat_data = bytes::Bytes::from_static(b"\x10Heartbeat acknowledged");

        self.batch_writer.queue_write(
            destination_addr,
            session_id,
            heartbeat_data,
            WritePriority::Immediate,
            true,
        ).await
            .map_err(|e| BatchError::ProcessingFailed(e.to_string()))
    }

    // Метод для отправки произвольного пакета
    pub async fn send_packet(
        &self,
        destination_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        packet_type: u8,
        data: &[u8],
        priority: WritePriority,
    ) -> Result<(), BatchError> {
        let mut packet_data = Vec::with_capacity(1 + data.len());
        packet_data.push(packet_type);
        packet_data.extend_from_slice(data);

        self.batch_writer.queue_write(
            destination_addr,
            session_id,
            bytes::Bytes::from(packet_data),
            priority,
            true,
        ).await
            .map_err(|e| BatchError::ProcessingFailed(e.to_string()))
    }

    pub async fn shutdown(&self) {
        info!("Shutting down Phantom Batch System...");

        // Отправляем события завершения
        let _ = self.send_reader_event(BatchReaderEvent::ConnectionClosed {
            source_addr: "0.0.0.0:0".parse().unwrap(),
            reason: "System shutdown".to_string(),
        }).await;

        info!("Phantom Batch System shutdown complete");
    }
}

// Реализация Clone
impl Clone for PhantomBatchSystem {
    fn clone(&self) -> Self {
        let buffer_pool_config = BufferPoolConfig::default();
        let reader_config = BatchReaderConfig::default();
        let writer_config = BatchWriterConfig::default();
        let crypto_config = BatchCryptoConfig::default();
        let _dispatcher_config = PacketBatchDispatcherConfig::default();

        // Создаем новые каналы
        let (reader_events_tx, reader_events_rx) = mpsc::channel(1000);
        let (writer_events_tx, writer_events_rx) = mpsc::channel(1000);

        // Создаем новые компоненты
        let buffer_pool = Arc::new(UnifiedBufferPool::new(buffer_pool_config));

        let batch_reader = Arc::new(BatchReader::new(reader_config, reader_events_tx.clone()));
        let batch_writer = Arc::new(BatchWriter::new(writer_config, writer_events_tx.clone()));

        let crypto_batch_processor = Arc::new(CryptoBatchProcessor::new(crypto_config));

        // Создаем новый packet service
        let packet_service = Arc::new(PhantomPacketService::new(
            self.session_manager.clone(),
            {
                use crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;
                Arc::new(ConnectionHeartbeatManager::new(
                    self.session_manager.clone(),
                    self.monitor.clone(),
                ))
            },
        ));

        // Для клона не создаем полный диспетчер - просто клонируем существующий
        let packet_dispatcher = self.packet_dispatcher.clone();

        // Запускаем обработчики событий для клона
        Self::start_event_handlers(
            reader_events_rx,
            writer_events_rx,
            packet_dispatcher.clone(),
            packet_service.clone(),
            batch_writer.clone(),
            self.monitor.clone(),
            self.session_manager.clone(),
            self.crypto.clone(),
        );

        Self {
            batch_reader,
            batch_writer,
            crypto_batch_processor,
            packet_dispatcher,
            buffer_pool,
            packet_service,
            reader_events_tx,
            writer_events_tx,
            monitor: self.monitor.clone(),
            session_manager: self.session_manager.clone(),
            crypto: self.crypto.clone(),
        }
    }
}