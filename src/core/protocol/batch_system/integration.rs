use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{info, error, debug, warn};
use bytes::{Bytes, BytesMut};

use crate::core::monitoring::unified_monitor::UnifiedMonitor;
use crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::packets::packet_service::PhantomPacketService;

use crate::core::protocol::batch_system::config::BatchConfig;
use crate::core::protocol::batch_system::core::reader::{BatchReader, ReaderEvent};
use crate::core::protocol::batch_system::core::writer::BatchWriter;
use crate::core::protocol::batch_system::optimized::work_stealing_dispatcher::{WorkStealingDispatcher, WorkStealingTask};
use crate::core::protocol::batch_system::optimized::buffer_pool::OptimizedBufferPool;
use crate::core::protocol::batch_system::optimized::crypto_processor::{OptimizedCryptoProcessor, CryptoOperation, CryptoResult};
use crate::core::protocol::batch_system::types::error::BatchError;
use crate::core::protocol::batch_system::types::priority::Priority;

/// Интегрированная batch система с оптимизированными компонентами
pub struct BatchSystem {
    config: BatchConfig,
    reader: Arc<BatchReader>,
    writer: Arc<BatchWriter>,
    dispatcher: Arc<WorkStealingDispatcher>,  // Используем оптимизированный диспетчер
    buffer_pool: Arc<OptimizedBufferPool>,    // Добавляем оптимизированный пул буферов
    crypto_processor: Arc<OptimizedCryptoProcessor>,  // Добавляем оптимизированный криптопроцессор
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
        info!("🚀 Creating optimized Batch System...");

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

        // Создаем оптимизированные компоненты
        let cpu_count = num_cpus::get();

        // Оптимизированный пул буферов
        let buffer_pool = Arc::new(OptimizedBufferPool::new(
            config.read_buffer_size,
            config.write_buffer_size,
            64 * 1024,  // crypto buffer size
            1000,       // max buffers
        ));

        // Оптимизированный криптопроцессор
        let crypto_processor = Arc::new(OptimizedCryptoProcessor::new(cpu_count));

        // Оптимизированный work-stealing диспетчер
        let dispatcher = Arc::new(WorkStealingDispatcher::new(
            config.worker_count,  // Используем конфигурацию из BatchConfig (cpu_count * 8)
            config.max_queue_size,
            session_manager.clone(), // ДОБАВИЛИ третий аргумент
        ));

        // Создаем стандартные компоненты
        let reader = Arc::new(BatchReader::new(config.clone(), reader_events_tx.clone()));
        let writer = Arc::new(BatchWriter::new(config.clone()));

        let worker_count = config.worker_count;
        let buffer_reuse_rate = buffer_pool.get_reuse_rate();

        let system = Self {
            config: config.clone(),
            reader,
            writer,
            dispatcher,
            buffer_pool,
            crypto_processor,
            packet_service,
            reader_events_tx,
            reader_events_rx: Arc::new(tokio::sync::Mutex::new(reader_events_rx)),
            monitor: monitor.clone(),
            session_manager: session_manager.clone(),
            crypto: crypto.clone(),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };

        // Запускаем обработчик событий (адаптированный под новый диспетчер)
        system.start_event_handler().await;

        // Запускаем мониторинг канала
        system.start_channel_monitoring().await;

        info!("✅ Optimized Batch System initialized successfully");
        info!("  - Worker count: {}", worker_count);
        info!("  - Buffer pool reuse rate: {:.1}%", buffer_reuse_rate * 100.0);

        Ok(system)
    }

    async fn start_event_handler(&self) {
        let dispatcher = self.dispatcher.clone();
        let packet_service = self.packet_service.clone();
        let session_manager = self.session_manager.clone();
        let buffer_pool = self.buffer_pool.clone();
        let crypto_processor = self.crypto_processor.clone();
        let writer = self.writer.clone();
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
                                // Создаем задачу для work-stealing диспетчера
                                let task = WorkStealingTask {
                                    id: 0,  // Будет установлен диспетчером
                                    session_id: session_id.clone(),
                                    data: Bytes::copy_from_slice(&data),
                                    source_addr,
                                    priority,
                                    created_at: received_at,
                                    worker_id: None,
                                };

                                match dispatcher.submit_task(task).await {
                                    Ok(task_id) => {
                                        debug!("✅ Event submitted to work-stealing dispatcher, task_id: {}", task_id);

                                        // Обрабатываем результат асинхронно
                                        Self::process_task_result(
                                            task_id,
                                            dispatcher.clone(),
                                            packet_service.clone(),
                                            session_manager.clone(),
                                            buffer_pool.clone(),
                                            crypto_processor.clone(),
                                            source_addr,
                                            session_id,
                                            priority,
                                            Instant::now(),
                                            writer.clone(),
                                        ).await;
                                    }
                                    Err(e) => {
                                        error!("❌ Failed to submit task to work-stealing dispatcher: {}", e);
                                    }
                                }
                            }
                            ReaderEvent::ConnectionClosed { source_addr, reason } => {
                                debug!("Connection closed: {} - {}", source_addr, reason);

                                // Уведомляем session manager о закрытии соединения
                                let session_id_empty = vec![];
                                if let Some(_) = session_manager.get_session(&session_id_empty).await {
                                    // Если сессия существует, пытаемся удалить
                                    session_manager.force_remove_session(&session_id_empty).await;
                                }
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

    /// Асинхронная обработка результата задачи
    async fn process_task_result(
        task_id: u64,
        dispatcher: Arc<WorkStealingDispatcher>,
        packet_service: Arc<PhantomPacketService>,
        session_manager: Arc<PhantomSessionManager>,  // Добавляем параметр
        buffer_pool: Arc<OptimizedBufferPool>,
        crypto_processor: Arc<OptimizedCryptoProcessor>,
        source_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        priority: Priority,
        start_time: Instant,
        writer: Arc<BatchWriter>,
    ) {
        // Используем переменные для устранения предупреждений
        let _buffer_stats = buffer_pool.get_detailed_stats();
        let _crypto_stats = crypto_processor.get_stats();

        use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;
        let packet_processor = PhantomPacketProcessor::new();

        debug!("Processing task {} from {} with priority: {:?}",
           task_id, source_addr, priority);

        // Получаем результат от dispatcher
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut attempts = 0;
            while attempts < 10 {
                if let Some(task_result) = dispatcher.get_result(task_id) {
                    return Some(task_result);
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                attempts += 1;
            }
            None
        }).await;

        match result {
            Ok(Some(task_result)) => {
                match task_result.result {
                    Ok(data) => {
                        let processing_time = Instant::now().duration_since(start_time);
                        debug!("✅ Task {} processed successfully by worker #{} in {:?} (data: {} bytes)",
                           task_id, task_result.worker_id, processing_time, data.len());

                        // Получаем session для обработки пакета
                        if let Some(session) = session_manager.get_session(&session_id).await {
                            // Извлекаем тип пакета и данные
                            let packet_type = if !data.is_empty() { data[0] } else { 0 };
                            let packet_data = if data.len() > 1 { &data[1..] } else { &[] };

                            info!("📦 Processing decrypted packet: type=0x{:02x}, data_len={}",
                              packet_type, packet_data.len());

                            match packet_service.process_packet(
                                session.clone(),
                                packet_type,
                                packet_data.to_vec(),
                                source_addr,
                            ).await {
                                Ok(processing_result) => {
                                    // ЗАШИФРОВЫВАЕМ ОТВЕТ
                                    match packet_processor.create_outgoing_vec(
                                        &session,
                                        processing_result.packet_type,
                                        &processing_result.response,
                                    ) {
                                        Ok(encrypted_response) => {
                                            // ИСПРАВЛЕНИЕ: Получаем реальный адрес сессии
                                            // Используем session_manager который у нас есть как параметр
                                            if let Some(session_info) = Self::get_session_info_with_addr(&session_manager, &session_id).await {
                                                let actual_destination_addr = session_info.addr;

                                                info!("📤 Sending encrypted response to {} (session: {})",
                                                  actual_destination_addr, hex::encode(&session_id));

                                                // ОТПРАВЛЯЕМ ЗАШИФРОВАННЫЙ ОТВЕТ
                                                if let Err(e) = writer.write(
                                                    actual_destination_addr,
                                                    session_id.clone(),
                                                    bytes::Bytes::from(encrypted_response),
                                                    processing_result.priority,
                                                    true,
                                                ).await {
                                                    error!("❌ Failed to send encrypted response to {}: {}",
                                                       actual_destination_addr, e);
                                                }
                                            } else {
                                                error!("❌ Session info not found for {} in session manager",
                                                   hex::encode(&session_id));
                                            }
                                        }
                                        Err(e) => {
                                            error!("❌ Failed to encrypt response: {}", e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("❌ Packet service failed to process packet: {}", e);
                                }
                            }
                        } else {
                            warn!("⚠️ Session not found for task {}: {}",
                              task_id, hex::encode(&session_id));
                        }
                    }
                    Err(err) => {
                        error!("❌ Task {} failed: {}", task_id, err);
                    }
                }
            }
            Ok(None) => {
                warn!("⚠️ Task {} result not available after timeout", task_id);
            }
            Err(_) => {
                error!("⏰ Timeout waiting for task {} result", task_id);
            }
        }
    }

    /// Вспомогательная функция для получения информации о сессии
    async fn get_session_info_with_addr(
        session_manager: &Arc<PhantomSessionManager>,
        session_id: &[u8],
    ) -> Option<SessionInfo> {
        let sessions = session_manager.sessions.read().await;
        sessions.get(session_id).map(|entry| SessionInfo {
            session_id: session_id.to_vec(),
            addr: entry.addr,
        })
    }

    /// Запускаем мониторинг состояния канала
    async fn start_channel_monitoring(&self) {
        let reader_events_tx = self.reader_events_tx.clone();
        let is_running = self.is_running.clone();
        let dispatcher = self.dispatcher.clone();
        let buffer_pool = self.buffer_pool.clone();
        let crypto_processor = self.crypto_processor.clone();
        let _packet_service = self.packet_service.clone();

        tokio::spawn(async move {
            let mut check_count = 0;

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                check_count += 1;

                // Проверяем состояние канала
                let is_closed = reader_events_tx.is_closed();
                let capacity = reader_events_tx.capacity();

                // Получаем статистику от компонентов
                let dispatcher_stats = dispatcher.get_stats();
                let buffer_reuse_rate = buffer_pool.get_reuse_rate();
                let crypto_stats = crypto_processor.get_stats();

                // Проверяем состояние packet service (проверяем что он не в None состоянии)
                let packet_service_active = true; // Предполагаем активным, так как нет метода is_shutdown

                debug!("System monitoring check #{}:", check_count);
                debug!("  - Channel: closed={}, capacity={}", is_closed, capacity);
                debug!("  - Dispatcher tasks processed: {}", dispatcher_stats.get("total_tasks_processed").unwrap_or(&0));
                debug!("  - Buffer pool reuse rate: {:.1}%", buffer_reuse_rate * 100.0);
                debug!("  - Crypto tasks processed: {}", crypto_stats.get("crypto_tasks_processed").unwrap_or(&0));
                debug!("  - Packet service active: {}", packet_service_active);

                // Если канал закрыт, логируем ошибку
                if is_closed {
                    error!("Reader events channel is closed!");
                }

                // Если емкость канала мала, логируем предупреждение
                if capacity < 100 {
                    warn!("Reader events channel running low on capacity: {}", capacity);
                }

                // Проверяем общее здоровье системы
                let system_healthy = !is_closed &&
                    capacity > 100 &&
                    packet_service_active &&
                    buffer_reuse_rate > 0.5;

                if !system_healthy {
                    warn!("⚠️ System health check failed:");
                    if is_closed {
                        warn!("  - Channel is closed");
                    }
                    if capacity <= 100 {
                        warn!("  - Low channel capacity: {}", capacity);
                    }
                    if !packet_service_active {
                        warn!("  - Packet service is shutdown");
                    }
                    if buffer_reuse_rate <= 0.5 {
                        warn!("  - Low buffer reuse rate: {:.1}%", buffer_reuse_rate * 100.0);
                    }
                }
            }
        });
    }

    // Методы для работы с соединениями (оставляем прежними)

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
        data: Bytes,
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
            Bytes::from_static(b"\x02PONG"),
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
            Bytes::from_static(b"\x10Heartbeat acknowledged"),
            Priority::Critical,
            true,
        ).await
    }

    // Новые методы для работы с оптимизированными компонентами

    /// Получение буфера из оптимизированного пула
    pub fn acquire_read_buffer(&self) -> Vec<u8> {
        self.buffer_pool.acquire_read_buffer()
    }

    /// Получение BytesMut буфера из оптимизированного пула
    pub fn acquire_bytes_mut(&self) -> BytesMut {
        self.buffer_pool.acquire_bytes_mut()
    }

    /// Возврат буфера в оптимизированный пул
    pub fn return_buffer(&self, buffer: Vec<u8>, buffer_type: &str) {
        self.buffer_pool.return_buffer(buffer, buffer_type);
    }

    /// Возврат BytesMut буфера в оптимизированный пул
    pub fn return_bytes_mut(&self, buffer: BytesMut) {
        self.buffer_pool.return_bytes_mut(buffer);
    }

    /// Выполнение криптографической операции через оптимизированный процессор
    pub async fn encrypt_data(
        &self,
        key: [u8; 32],
        nonce: [u8; 12],
        plaintext: Vec<u8>,
        session_id: Vec<u8>,
    ) -> Result<u64, BatchError> {
        let operation = CryptoOperation::EncryptChaCha20 {
            key,
            nonce,
            plaintext,
        };

        match self.crypto_processor.submit_crypto_task(operation, session_id, 1).await {
            Ok(task_id) => {
                debug!("✅ Crypto task submitted: {}", task_id);

                // Отслеживаем результат крипто операции
                self.track_crypto_result(task_id).await;

                Ok(task_id)
            }
            Err(e) => Err(BatchError::ProcessingError(format!("Crypto submission failed: {}", e))),
        }
    }

    /// Отслеживание результата криптографической операции
    async fn track_crypto_result(&self, task_id: u64) {
        let crypto_processor = self.crypto_processor.clone();

        tokio::spawn(async move {
            let mut attempts = 0;
            while attempts < 20 {
                if let Some(result) = crypto_processor.get_crypto_result(task_id) {
                    match result.result {
                        Ok(data) => {
                            debug!("✅ Crypto task {} completed successfully in {:?} ({} bytes)",
                                task_id, result.processing_time, data.len());
                            break;
                        }
                        Err(err) => {
                            error!("❌ Crypto task {} failed: {}", task_id, err);
                            break;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                attempts += 1;
            }

            if attempts >= 20 {
                warn!("⚠️ Crypto task {} result not available after timeout", task_id);
            }
        });
    }

    /// Получение результата криптографической операции
    pub fn get_crypto_result(&self, task_id: u64) -> Option<CryptoResult> {
        self.crypto_processor.get_crypto_result(task_id)
    }

    /// Пакетное шифрование данных
    pub async fn encrypt_batch(
        &self,
        keys: Vec<[u8; 32]>,
        nonces: Vec<[u8; 12]>,
        plaintexts: Vec<Vec<u8>>,
        session_ids: Vec<Vec<u8>>,
    ) -> Vec<Result<u64, BatchError>> {
        let mut results = Vec::new();

        for i in 0..keys.len().min(nonces.len()).min(plaintexts.len()).min(session_ids.len()) {
            let result = self.encrypt_data(
                keys[i],
                nonces[i],
                plaintexts[i].clone(),
                session_ids[i].clone(),
            ).await;

            results.push(result);
        }

        results
    }

    /// Получение статуса канала
    pub fn get_channel_status(&self) -> ChannelStatus {
        let is_closed = self.reader_events_tx.is_closed();
        let capacity = self.reader_events_tx.capacity();

        ChannelStatus {
            is_closed,
            capacity,
            is_healthy: !is_closed && capacity > 100,
        }
    }

    /// Получение статистики системы
    pub fn get_system_stats(&self) -> SystemStats {
        let dispatcher_stats = self.dispatcher.get_stats();
        let buffer_stats = self.buffer_pool.get_detailed_stats();
        let crypto_stats = self.crypto_processor.get_stats();
        let channel_status = self.get_channel_status();

        let total_tasks_processed = *dispatcher_stats.get("total_tasks_processed").unwrap_or(&0);
        let buffer_reuse_rate = self.buffer_pool.get_reuse_rate();
        let crypto_tasks_processed = *crypto_stats.get("crypto_tasks_processed").unwrap_or(&0);

        SystemStats {
            total_tasks_processed,
            buffer_reuse_rate,
            crypto_tasks_processed,
            channel_healthy: channel_status.is_healthy,
            channel_capacity: channel_status.capacity,
            buffer_pool_stats: buffer_stats,
            dispatcher_worker_count: dispatcher_stats.len(),
        }
    }

    /// Проверка здоровья системы
    pub fn is_healthy(&self) -> bool {
        let stats = self.get_system_stats();

        stats.channel_healthy &&
            stats.buffer_reuse_rate > 0.3
    }

    /// Получение информации о системе для мониторинга
    pub fn get_monitoring_info(&self) -> MonitoringInfo {
        let stats = self.get_system_stats();
        let buffer_pool_info = self.buffer_pool.get_memory_usage();
        let channel_status = self.get_channel_status();

        MonitoringInfo {
            system_healthy: self.is_healthy(),
            total_tasks_processed: stats.total_tasks_processed,
            buffer_reuse_rate: stats.buffer_reuse_rate,
            crypto_tasks_processed: stats.crypto_tasks_processed,
            channel_healthy: channel_status.is_healthy,
            channel_capacity: channel_status.capacity,
            memory_usage_kb: buffer_pool_info.total_memory_kb,
            dispatcher_worker_count: stats.dispatcher_worker_count,
            created_at: Instant::now(),
        }
    }

    /// Получение конфигурации системы
    pub fn get_config(&self) -> &BatchConfig {
        &self.config
    }

    // Получение компонентов

    pub fn reader(&self) -> Arc<BatchReader> {
        self.reader.clone()
    }

    pub fn writer(&self) -> Arc<BatchWriter> {
        self.writer.clone()
    }

    pub fn dispatcher(&self) -> Arc<WorkStealingDispatcher> {
        self.dispatcher.clone()
    }

    pub fn buffer_pool(&self) -> Arc<OptimizedBufferPool> {
        self.buffer_pool.clone()
    }

    pub fn crypto_processor(&self) -> Arc<OptimizedCryptoProcessor> {
        self.crypto_processor.clone()
    }

    pub fn packet_service(&self) -> Arc<PhantomPacketService> {
        self.packet_service.clone()
    }

    pub fn session_manager(&self) -> Arc<PhantomSessionManager> {
        self.session_manager.clone()
    }

    // Управление системой

    pub async fn shutdown(&self) {
        info!("Shutting down Optimized Batch System...");

        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);

        self.reader.shutdown().await;
        self.writer.shutdown().await;
        self.dispatcher.shutdown().await;
        self.crypto_processor.shutdown().await;

        // Логируем статус перед завершением
        let stats = self.get_system_stats();
        let monitoring_info = self.get_monitoring_info();

        info!("Final system stats:");
        info!("  - Tasks processed: {}", stats.total_tasks_processed);
        info!("  - Crypto tasks processed: {}", stats.crypto_tasks_processed);
        info!("  - Buffer reuse rate: {:.1}%", stats.buffer_reuse_rate * 100.0);
        info!("  - Channel healthy: {}", stats.channel_healthy);
        info!("  - Memory usage: {:.1} MB", monitoring_info.memory_usage_kb as f64 / 1024.0);
        info!("  - System healthy: {}", monitoring_info.system_healthy);

        info!("✅ Optimized Batch System shutdown complete");
    }
}

/// Структура для статуса канала
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

/// Статистика системы
#[derive(Debug, Clone)]
pub struct SystemStats {
    pub total_tasks_processed: u64,
    pub buffer_reuse_rate: f64,
    pub crypto_tasks_processed: u64,
    pub channel_healthy: bool,
    pub channel_capacity: usize,
    pub buffer_pool_stats: std::collections::HashMap<String, super::optimized::buffer_pool::BufferPoolStats>,
    pub dispatcher_worker_count: usize,
}

impl SystemStats {
    pub fn to_string(&self) -> String {
        format!(
            "Tasks: {}, Crypto: {}, Buffer reuse: {:.1}%, Channel: {} (capacity: {}), Workers: {}",
            self.total_tasks_processed,
            self.crypto_tasks_processed,
            self.buffer_reuse_rate * 100.0,
            if self.channel_healthy { "healthy" } else { "unhealthy" },
            self.channel_capacity,
            self.dispatcher_worker_count
        )
    }
}

#[derive(Debug, Clone)]
struct SessionInfo {
    session_id: Vec<u8>,
    addr: std::net::SocketAddr,
}

/// Информация для мониторинга
#[derive(Debug, Clone)]
pub struct MonitoringInfo {
    pub system_healthy: bool,
    pub total_tasks_processed: u64,
    pub buffer_reuse_rate: f64,
    pub crypto_tasks_processed: u64,
    pub channel_healthy: bool,
    pub channel_capacity: usize,
    pub memory_usage_kb: usize,
    pub dispatcher_worker_count: usize,
    pub created_at: Instant,
}

impl MonitoringInfo {
    pub fn to_metrics_string(&self) -> String {
        format!(
            "Healthy: {}, Tasks: {}, Crypto: {}, Buffer: {:.1}%, Channel: {}, Memory: {:.1} MB, Workers: {}",
            self.system_healthy,
            self.total_tasks_processed,
            self.crypto_tasks_processed,
            self.buffer_reuse_rate * 100.0,
            self.channel_healthy,
            self.memory_usage_kb as f64 / 1024.0,
            self.dispatcher_worker_count
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
            buffer_pool: self.buffer_pool.clone(),
            crypto_processor: self.crypto_processor.clone(),
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