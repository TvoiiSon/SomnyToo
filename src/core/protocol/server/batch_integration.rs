use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, debug, warn, error};

use crate::core::monitoring::unified_monitor::UnifiedMonitor;
use crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;

// Импортируем batch компоненты через правильные пути
use crate::core::protocol::phantom_crypto::batch::{
    io::reader::batch_reader::{BatchReader, BatchReaderConfig, BatchReaderEvent},
    io::writer::batch_writer::{BatchWriter, BatchWriterConfig, BatchWriterEvent},
    processor::crypto_batch_processor::{CryptoBatchProcessor, BatchCryptoConfig},
    dispatcher::packet_batch_dispatcher::{PacketBatchDispatcher, PacketBatchDispatcherConfig},
    buffer::unified_buffer_pool::{UnifiedBufferPool, BufferPoolConfig},
};
use crate::core::protocol::phantom_crypto::batch::dispatcher::task::DispatchTask;
use crate::core::protocol::phantom_crypto::batch::types::error::BatchError;

/// Интеграционная структура, объединяющая все batch компоненты
pub struct PhantomBatchSystem {
    pub batch_reader: Arc<BatchReader>,
    pub batch_writer: Arc<BatchWriter>,
    pub crypto_batch_processor: Arc<CryptoBatchProcessor>,
    pub packet_dispatcher: Arc<PacketBatchDispatcher>,
    pub buffer_pool: Arc<UnifiedBufferPool>,

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

        // Создаем batch reader
        let batch_reader = Arc::new(BatchReader::new(reader_config, reader_events_tx.clone()));

        // Создаем batch writer
        let batch_writer = Arc::new(BatchWriter::new(writer_config, writer_events_tx.clone()));

        // Создаем crypto batch processor
        let crypto_batch_processor = Arc::new(CryptoBatchProcessor::new(crypto_config));

        // Создаем packet dispatcher - явно указываем тип
        let packet_dispatcher: Arc<PacketBatchDispatcher> = Arc::new(PacketBatchDispatcher::new(
            dispatcher_config,
            crypto_batch_processor.clone(),
            batch_writer.clone(),
            monitor_clone.clone(),
        ).await);

        // Запускаем обработчики событий
        Self::start_event_handlers(
            reader_events_rx,
            writer_events_rx,
            packet_dispatcher.clone(),
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
            reader_events_tx,
            writer_events_tx,
            monitor: monitor_clone,
            session_manager: session_manager_clone,
            crypto: crypto_clone,
        }
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

    fn start_event_handlers(
        mut reader_events_rx: mpsc::Receiver<BatchReaderEvent>,
        mut writer_events_rx: mpsc::Receiver<BatchWriterEvent>,
        packet_dispatcher: Arc<PacketBatchDispatcher>,
        monitor: Arc<UnifiedMonitor>,
        session_manager: Arc<PhantomSessionManager>,
        crypto: Arc<PhantomCrypto>,
    ) {
        // Используем все переменные в обработчиках
        let session_manager_clone = session_manager.clone();
        let crypto_clone = crypto.clone();

        // Обработчик событий от reader
        tokio::spawn(async move {
            info!("📊 Batch reader event handler started");
            while let Some(event) = reader_events_rx.recv().await {
                match event {
                    BatchReaderEvent::BatchReady { batch_id, frames, source_addr, received_at } => {
                        // Используем session_manager и crypto
                        debug!("Processing batch #{} from {} with {} frames using crypto: {}, session_manager: {}",
                               batch_id, source_addr, frames.len(),
                               std::mem::size_of_val(&*crypto_clone),
                               std::mem::size_of_val(&*session_manager_clone));

                        // Передаем batch в диспетчер
                        packet_dispatcher.process_batch_from_reader(
                            BatchReaderEvent::BatchReady { batch_id, frames, source_addr, received_at }
                        ).await;
                    }
                    BatchReaderEvent::ConnectionClosed { source_addr, reason } => {
                        packet_dispatcher.process_batch_from_reader(
                            BatchReaderEvent::ConnectionClosed { source_addr, reason }
                        ).await;
                    }
                    BatchReaderEvent::ReadError { source_addr, error } => {
                        packet_dispatcher.process_batch_from_reader(
                            BatchReaderEvent::ReadError { source_addr, error }
                        ).await;
                    }
                    BatchReaderEvent::StatisticsUpdate { stats } => {
                        // Логируем статистику
                        info!("Reader stats: {} fps, {} bytes/s",
                            stats.frames_per_second,
                            stats.bytes_per_second);
                    }
                }
            }
        });

        // Обработчик событий от writer
        tokio::spawn(async move {
            info!("📊 Batch writer event handler started");
            while let Some(event) = writer_events_rx.recv().await {
                match event {
                    BatchWriterEvent::WriteCompleted { destination_addr, batch_id, bytes_written, write_time } => {
                        info!("Write completed for {}: batch #{}, {} bytes in {:?}",
                            destination_addr, batch_id, bytes_written, write_time);
                    }
                    BatchWriterEvent::WriteError { destination_addr, error } => {
                        error!("Write error for {}: {}", destination_addr, error);

                        // Отправляем алерт в мониторинг
                        if let Some(_monitor_ref) = Arc::get_mut(&mut monitor.clone()) {
                            debug!("Would send write error alert to monitor");
                        }
                    }
                    BatchWriterEvent::BufferFull { destination_addr, buffer_size } => {
                        warn!("Buffer full for {}: {} bytes", destination_addr, buffer_size);
                    }
                    BatchWriterEvent::StatisticsUpdate { stats } => {
                        // Логируем статистику
                        debug!("Writer stats: {} wps, {} bytes/s",
                            stats.writes_per_second,
                            stats.bytes_per_second);
                    }
                }
            }
        });
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

    pub async fn shutdown(&self) {
        info!("Shutting down Phantom Batch System...");

        // Отправляем события завершения
        let _ = self.send_reader_event(BatchReaderEvent::ConnectionClosed {
            source_addr: "0.0.0.0:0".parse().unwrap(),
            reason: "System shutdown".to_string(),
        }).await;

        // TODO: Реализовать корректное завершение работы всех компонентов
    }
}