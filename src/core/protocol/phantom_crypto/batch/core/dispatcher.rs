use std::sync::Arc;
use std::time::{Instant, Duration};
use tokio::sync::{mpsc, RwLock, Mutex, Semaphore};
use bytes::BytesMut;
use tracing::{info, error};

use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;
use crate::core::protocol::packets::packet_service::PhantomPacketService;

use crate::core::protocol::phantom_crypto::batch::config::BatchConfig;
use crate::core::protocol::phantom_crypto::batch::types::error::BatchError;
use crate::core::protocol::phantom_crypto::batch::types::priority::Priority;
use crate::core::protocol::phantom_crypto::batch::core::writer::BatchWriter;

/// Задача для диспетчера
#[derive(Debug, Clone)]
pub struct DispatchTask {
    pub session_id: Vec<u8>,
    pub data: BytesMut,
    pub source_addr: std::net::SocketAddr,
    pub priority: Priority,
    pub received_at: Instant,
}

/// Результат обработки
#[derive(Debug, Clone)]
pub struct DispatchResult {
    pub session_id: Vec<u8>,
    pub destination_addr: std::net::SocketAddr,
    pub response_data: Option<BytesMut>,
    pub priority: Priority,
    pub processing_time: Duration,
}

/// Диспетчер пакетов
pub struct PacketDispatcher {
    config: BatchConfig,
    session_manager: Arc<PhantomSessionManager>,
    packet_service: Arc<PhantomPacketService>,
    packet_processor: PhantomPacketProcessor,
    batch_writer: Arc<BatchWriter>,

    // Очереди
    task_tx: Arc<mpsc::Sender<DispatchTask>>,
    task_rx: Arc<Mutex<mpsc::Receiver<DispatchTask>>>,
    result_tx: Arc<mpsc::Sender<DispatchResult>>,  // Добавили Arc
    result_rx: Arc<Mutex<mpsc::Receiver<DispatchResult>>>,  // Добавили Arc

    // Управление
    workers: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,
    backpressure: Arc<Semaphore>,
    is_running: Arc<std::sync::atomic::AtomicBool>,
}

impl PacketDispatcher {
    pub async fn new(
        config: BatchConfig,
        session_manager: Arc<PhantomSessionManager>,
        packet_service: Arc<PhantomPacketService>,
        batch_writer: Arc<BatchWriter>,
    ) -> Self {
        let (task_tx, task_rx) = mpsc::channel(config.max_queue_size);
        let (result_tx, result_rx) = mpsc::channel(1000);

        info!("🧩 Creating PacketDispatcher with config: {:?}", config);

        let dispatcher = Self {
            config: config.clone(),
            session_manager: session_manager.clone(),
            packet_service: packet_service.clone(),
            packet_processor: PhantomPacketProcessor::new(),
            batch_writer: batch_writer.clone(),
            task_tx: Arc::new(task_tx),
            task_rx: Arc::new(Mutex::new(task_rx)),
            result_tx: Arc::new(result_tx),  // Обернули в Arc
            result_rx: Arc::new(Mutex::new(result_rx)),  // Обернули в Arc
            workers: Arc::new(RwLock::new(Vec::new())),
            backpressure: Arc::new(Semaphore::new(config.max_queue_size)),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };

        info!("🔧 PacketDispatcher struct created");

        // Запускаем worker-ов
        dispatcher.start_workers().await;
        info!("👷 Workers started");

        // Запускаем обработчик результатов
        dispatcher.start_result_handler().await;
        info!("📨 Result handler started");

        info!("✅ PacketDispatcher initialized with {} workers",
            dispatcher.config.worker_count);

        dispatcher
    }

    pub async fn submit_task(&self, task: DispatchTask) -> Result<(), BatchError> {
        info!("📤 Submitting task from {} session: {}",
            task.source_addr, hex::encode(&task.session_id));

        // Проверяем backpressure
        let permit = self.backpressure.clone()
            .try_acquire_owned()
            .map_err(|_| BatchError::Backpressure)?;

        match self.task_tx.send(task).await {  // Используем Arc-wrapped sender
            Ok(_) => {
                info!("✅ Task submitted successfully");
                Ok(())
            }
            Err(e) => {
                error!("❌ Failed to submit task: {}", e);
                drop(permit);
                Err(BatchError::ProcessingError(e.to_string()))
            }
        }
    }

    async fn start_workers(&self) {
        info!("🚀 Starting {} dispatcher workers...", self.config.worker_count);

        let mut handles = Vec::new();
        for worker_id in 0..self.config.worker_count {
            info!("👷 Spawning worker #{}...", worker_id);
            let handle = self.spawn_worker(worker_id).await;
            handles.push(handle);
        }

        // Сохраняем handles
        *self.workers.write().await = handles;

        info!("✅ All {} dispatcher workers started", self.config.worker_count);
    }

    async fn spawn_worker(&self, worker_id: usize) -> tokio::task::JoinHandle<()> {
        let dispatcher = self.clone();

        let handle = tokio::spawn(async move {
            info!("👷 Dispatcher worker #{} started", worker_id);

            // КАЖДЫЙ worker получает СВОЙ receiver из ОБЩЕГО канала
            let task_rx = dispatcher.task_rx.clone();
            let mut task_receiver = task_rx.lock().await;

            info!("📭 Worker #{} got task receiver", worker_id);

            while dispatcher.is_running.load(std::sync::atomic::Ordering::Relaxed) {
                info!("⏳ Worker #{} waiting for task...", worker_id);

                match task_receiver.recv().await {
                    Some(task) => {
                        info!("📥 Worker #{} received task from {}", worker_id, task.source_addr);

                        match dispatcher.process_task(&task).await {
                            Ok(result) => {
                                info!("✅ Worker #{} processed task successfully", worker_id);

                                if let Some(response) = result.response_data {
                                    let dispatch_result = DispatchResult {
                                        session_id: result.session_id,
                                        destination_addr: result.destination_addr,
                                        response_data: Some(response),
                                        priority: result.priority,
                                        processing_time: result.processing_time,
                                    };

                                    info!("📤 Worker #{} sending result to result handler", worker_id);

                                    // Используем Arc-wrapped sender
                                    match dispatcher.result_tx.send(dispatch_result).await {
                                        Ok(_) => info!("✅ Worker #{} result sent successfully", worker_id),
                                        Err(e) => error!("❌ Worker #{} failed to send result: {}", worker_id, e),
                                    }
                                }

                                // Освобождаем backpressure permit
                                dispatcher.backpressure.add_permits(1);
                                info!("🔓 Worker #{} released backpressure", worker_id);
                            }
                            Err(e) => {
                                error!("❌ Worker #{}: Task processing error: {}", worker_id, e);
                                dispatcher.backpressure.add_permits(1);
                            }
                        }
                    }
                    None => {
                        info!("📭 Worker #{}: channel closed", worker_id);
                        break;
                    }
                }
            }

            info!("👋 Dispatcher worker #{} stopped", worker_id);
        });

        handle
    }

    async fn process_task(&self, task: &DispatchTask) -> Result<DispatchResult, BatchError> {
        let start_time = Instant::now();

        info!("📥 START Processing task from {} session: {} ({} bytes)",
    task.source_addr, hex::encode(&task.session_id), task.data.len());

        // Получаем сессию
        let session = match self.session_manager.get_session(&task.session_id).await {
            Some(session) => {
                info!("✅ Session found for {}", hex::encode(&task.session_id));
                session
            }
            None => {
                error!("❌ Session not found: {}", hex::encode(&task.session_id));
                return Err(BatchError::InvalidSession(
                    format!("Session not found: {}", hex::encode(&task.session_id))
                ));
            }
        };

        info!("🔓 Attempting to decrypt packet...");

        // Обрабатываем входящий пакет
        match self.packet_processor.process_incoming_vec(&task.data, &session) {
            Ok((packet_type, decrypted_data)) => {
                info!("✅ DECRYPTED: packet_type=0x{:02x}, data_len={}",
            packet_type, decrypted_data.len());

                info!("📦 Processing through packet service...");

                // Обрабатываем через packet service
                match self.packet_service.process_packet(
                    session.clone(),
                    packet_type,
                    decrypted_data,
                    task.source_addr,
                ).await {
                    Ok(processing_result) => {
                        info!("🎯 Packet service processed: response_len={}, packet_type=0x{:02x}, priority={:?}",
                    processing_result.response.len(), processing_result.packet_type, processing_result.priority);

                        info!("🔒 Encrypting response...");

                        // Шифруем ответ
                        match self.packet_processor.create_outgoing_vec(
                            &session,
                            processing_result.packet_type, // Используем ТОТ ЖЕ packet_type!
                            &processing_result.response,
                        ) {
                            Ok(encrypted_response) => {
                                info!("✅ RESPONSE READY: {} bytes to {}",
                            encrypted_response.len(), task.source_addr);

                                // Отправляем через BatchWriter с правильным приоритетом
                                info!("📤 Sending response with priority: {:?}", processing_result.priority);

                                match self.batch_writer.write(
                                    task.source_addr,
                                    task.session_id.clone(),
                                    bytes::Bytes::from(encrypted_response.clone()),
                                    processing_result.priority, // Используем приоритет из результата
                                    true,
                                ).await {
                                    Ok(_) => {
                                        info!("✅ Response sent successfully to {}", task.source_addr);
                                        Ok(DispatchResult {
                                            session_id: task.session_id.clone(),
                                            destination_addr: task.source_addr,
                                            response_data: Some(BytesMut::from(&encrypted_response[..])),
                                            priority: processing_result.priority,
                                            processing_time: start_time.elapsed(),
                                        })
                                    }
                                    Err(e) => {
                                        error!("❌ Failed to send response via BatchWriter: {}", e);
                                        // Все равно возвращаем успех, так как пакет обработан
                                        Ok(DispatchResult {
                                            session_id: task.session_id.clone(),
                                            destination_addr: task.source_addr,
                                            response_data: Some(BytesMut::from(&encrypted_response[..])),
                                            priority: processing_result.priority,
                                            processing_time: start_time.elapsed(),
                                        })
                                    }
                                }
                            }
                            Err(e) => {
                                error!("❌ Encryption failed: {}", e);
                                Err(BatchError::Crypto(format!("Encryption failed: {}", e)))
                            }
                        }
                    }
                    Err(e) => {
                        error!("❌ Packet processing failed: {}", e);
                        Err(BatchError::ProcessingError(format!("Packet processing failed: {}", e)))
                    }
                }
            }
            Err(e) => {
                error!("❌ DECRYPTION FAILED for session {} from {}: {}",
            hex::encode(&task.session_id), task.source_addr, e);
                Err(BatchError::Crypto(format!("Decryption failed: {}", e)))
            }
        }
    }

    async fn start_result_handler(&self) {
        info!("🚀 Starting result handler...");

        let dispatcher = self.clone();

        tokio::spawn(async move {
            info!("📨 Result handler task started");

            // Берем receiver из Arc
            let result_rx = dispatcher.result_rx.clone();
            let mut result_receiver = result_rx.lock().await;

            info!("🔓 Result handler got receiver lock");
            info!("⏳ Result handler waiting for results...");

            while let Some(result) = result_receiver.recv().await {
                info!("📨 Result handler received result for {}", result.destination_addr);

                if let Some(response_data) = result.response_data {
                    info!("📤 Sending response to {} ({} bytes)",
                        result.destination_addr, response_data.len());

                    // Отправляем ответ через BatchWriter
                    match dispatcher.batch_writer.write(
                        result.destination_addr,
                        result.session_id.clone(),
                        bytes::Bytes::from(response_data),
                        result.priority,
                        true,
                    ).await {
                        Ok(_) => {
                            info!("✅ Response sent to {}", result.destination_addr);
                        }
                        Err(e) => {
                            error!("❌ Failed to send response to {}: {}",
                                result.destination_addr, e);
                        }
                    }
                }
            }

            info!("👋 Result handler stopped");
        });
    }

    pub async fn shutdown(&self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);

        // Ждем завершения worker-ов
        let workers = self.workers.write().await;
        for worker in workers.iter() {
            worker.abort();
        }

        info!("PacketDispatcher shutdown completed");
    }
}

impl Clone for PacketDispatcher {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            session_manager: self.session_manager.clone(),
            packet_service: self.packet_service.clone(),
            packet_processor: PhantomPacketProcessor::new(),
            batch_writer: self.batch_writer.clone(),
            task_tx: self.task_tx.clone(),
            task_rx: self.task_rx.clone(),
            result_tx: self.result_tx.clone(),  // Клонируем Arc
            result_rx: self.result_rx.clone(),  // Клонируем Arc
            workers: Arc::new(RwLock::new(Vec::new())),
            backpressure: Arc::new(Semaphore::new(self.config.max_queue_size)),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
}