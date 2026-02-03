use std::sync::Arc;
use std::time::{Instant};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, RwLock, Semaphore};
use bytes::Bytes;
use tracing::{info, debug, error, warn};

use crate::core::protocol::packets::frame_writer;

use crate::core::protocol::phantom_crypto::batch::config::BatchConfig;
use crate::core::protocol::phantom_crypto::batch::types::error::BatchError;
use crate::core::protocol::phantom_crypto::batch::types::priority::Priority;

/// Задача записи
#[derive(Debug, Clone)]
pub struct WriteTask {
    pub destination_addr: std::net::SocketAddr,
    pub session_id: Vec<u8>,
    pub data: Bytes,
    pub priority: Priority,
    pub requires_flush: bool,
}

/// Писатель данных
pub struct BatchWriter {
    config: BatchConfig,
    connections: Arc<RwLock<Vec<ConnectionWriter>>>,
    task_tx: broadcast::Sender<WriteTask>,  // broadcast вместо mpsc
    backpressure: Arc<Semaphore>,
    is_running: Arc<std::sync::atomic::AtomicBool>,
}

struct ConnectionWriter {
    destination_addr: std::net::SocketAddr,
    session_id: Vec<u8>,
    write_stream: Box<dyn AsyncWrite + Unpin + Send + Sync>,
    last_write_time: Instant,
    is_active: bool,
}

impl BatchWriter {
    pub fn new(config: BatchConfig) -> Self {
        let (task_tx, _) = broadcast::channel(config.max_pending_writes);

        Self {
            config: config.clone(),
            connections: Arc::new(RwLock::new(Vec::new())),
            task_tx,
            backpressure: Arc::new(Semaphore::new(config.max_pending_writes)),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    pub async fn register_connection(
        &self,
        destination_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        write_stream: Box<dyn AsyncWrite + Unpin + Send + Sync>,
    ) -> Result<(), BatchError> {
        let writer = ConnectionWriter {
            destination_addr,
            session_id: session_id.clone(),
            write_stream,
            last_write_time: Instant::now(),
            is_active: true,
        };

        {
            let mut connections = self.connections.write().await;
            connections.push(writer);
        }

        self.start_writer_for_connection(destination_addr).await?;

        info!("Registered writer connection: {} session: {}",
            destination_addr, hex::encode(&session_id));

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
        info!("📝 BatchWriter: Writing {} bytes to {} session: {}",
        data.len(), destination_addr, hex::encode(&session_id));

        // Проверяем backpressure
        let permit = self.backpressure.clone()
            .try_acquire_owned()
            .map_err(|_| {
                warn!("⚠️ Backpressure for {}", destination_addr);
                BatchError::Backpressure
            })?;

        let task = WriteTask {
            destination_addr,
            session_id,
            data,
            priority,
            requires_flush,
        };

        info!("📊 Priority: {:?}, requires_flush: {}", priority, requires_flush);

        // Немедленная запись для критических пакетов
        if priority.is_critical() {
            info!("⚡ Immediate write for critical packet to {}", destination_addr);
            let result = self.write_immediate(task).await;
            drop(permit);
            return result;
        } else {
            // Буферизированная запись через broadcast
            info!("📦 Buffered write to {}", destination_addr);
            match self.task_tx.send(task) {
                Ok(_) => {
                    info!("✅ Task sent to writer broadcast channel");
                    Ok(())
                }
                Err(e) => {
                    error!("❌ Failed to send task: {}", e);
                    drop(permit);
                    Err(BatchError::ProcessingError(e.to_string()))
                }
            }
        }
    }

    async fn write_immediate(&self, task: WriteTask) -> Result<(), BatchError> {
        info!("⚡ IMMEDIATE WRITE START: {} bytes to {}",
        task.data.len(), task.destination_addr);

        let mut connections = self.connections.write().await;

        if let Some(writer) = connections.iter_mut()
            .find(|w| w.destination_addr == task.destination_addr && w.is_active) {

            info!("✅ Found active writer for {}", task.destination_addr);

            match tokio::time::timeout(
                self.config.write_timeout,
                frame_writer::write_frame(&mut writer.write_stream, &task.data),
            ).await {
                Ok(Ok(_)) => {
                    info!("✅ FRAME WRITTEN: {} bytes", task.data.len());

                    if task.requires_flush {
                        info!("🌀 Flushing stream...");
                        writer.write_stream.flush().await
                            .map_err(BatchError::Io)?;
                        info!("✅ Stream flushed");
                    }

                    writer.last_write_time = Instant::now();
                    info!("✅ IMMEDIATE WRITE COMPLETE to {} session {}: {} bytes",
                    task.destination_addr, hex::encode(&writer.session_id), task.data.len());

                    Ok(())
                }
                Ok(Err(e)) => {
                    writer.is_active = false;
                    error!("❌ Immediate write failed for session {}: {}",
                    hex::encode(&writer.session_id), e);
                    Err(BatchError::ProcessingError(e.to_string()))
                }
                Err(_) => {
                    writer.is_active = false;
                    error!("⏰ Immediate write timeout for session {}",
                    hex::encode(&writer.session_id));
                    Err(BatchError::Timeout)
                }
            }
        } else {
            error!("❌ Connection not found for immediate write to {}",
            task.destination_addr);
            Err(BatchError::ConnectionError("Connection not found".to_string()))
        }
    }

    async fn start_writer_for_connection(&self, destination_addr: std::net::SocketAddr) -> Result<(), BatchError> {
        let connections = self.connections.clone();
        let config = self.config.clone();
        let is_running = self.is_running.clone();
        let backpressure = self.backpressure.clone();

        // Создаем отдельный receiver для этого writer task
        let mut task_rx = self.task_tx.subscribe();

        tokio::spawn(async move {
            let mut pending_tasks: Vec<WriteTask> = Vec::with_capacity(config.batch_size);

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::select! {
                    Ok(task) = task_rx.recv() => {
                        if task.destination_addr == destination_addr {
                            pending_tasks.push(task);

                            // Проверяем, нужно ли сбрасывать батч
                            if pending_tasks.len() >= config.batch_size {
                                if let Err(e) = Self::process_batch(
                                    &connections,
                                    destination_addr,
                                    &mut pending_tasks,
                                    &config
                                ).await {
                                    error!("Batch write error: {}", e);
                                }
                            }
                        }
                    }
                    _ = tokio::time::sleep(config.flush_interval) => {
                        // Таймер сброса
                        if !pending_tasks.is_empty() {
                            if let Err(e) = Self::process_batch(
                                &connections,
                                destination_addr,
                                &mut pending_tasks,
                                &config
                            ).await {
                                error!("Batch write error: {}", e);
                            }
                        }
                    }
                }

                // Освобождаем backpressure permits для обработанных задач
                if !pending_tasks.is_empty() {
                    backpressure.add_permits(pending_tasks.len());
                    pending_tasks.clear();
                }
            }
        });

        Ok(())
    }

    async fn process_batch(
        connections: &Arc<RwLock<Vec<ConnectionWriter>>>,
        destination_addr: std::net::SocketAddr,
        tasks: &mut Vec<WriteTask>,
        config: &BatchConfig,
    ) -> Result<(), BatchError> {
        if tasks.is_empty() {
            return Ok(());
        }

        let mut connections = connections.write().await;
        let writer_opt = connections.iter_mut()
            .find(|w| w.destination_addr == destination_addr && w.is_active);

        if let Some(writer) = writer_opt {
            // Логируем session_id для отладки
            let session_id_hex = hex::encode(&writer.session_id);
            debug!("Batch writing to {} session: {}",
                destination_addr, session_id_hex);

            // Сортируем задачи по приоритету (Critical first)
            tasks.sort_by(|a, b| b.priority.cmp(&a.priority));

            // Объединяем данные
            let mut combined_data = Vec::new();
            let mut requires_flush = false;
            let mut total_bytes = 0;

            for task in tasks.iter() {
                combined_data.extend_from_slice(&task.data);
                total_bytes += task.data.len();
                if task.requires_flush {
                    requires_flush = true;
                }
            }

            let data_bytes = Bytes::from(combined_data);

            // Логируем информацию о батче с session_id
            debug!("Batch for session {}: {} tasks, {} bytes, highest priority: {:?}",
                session_id_hex, tasks.len(), total_bytes,
                tasks.first().map(|t| t.priority).unwrap_or(Priority::Normal));

            match tokio::time::timeout(
                config.write_timeout * (tasks.len() as u32).max(1),
                frame_writer::write_frame(&mut writer.write_stream, &data_bytes),
            ).await {
                Ok(Ok(_)) => {
                    writer.last_write_time = Instant::now();

                    // Flush если требуется
                    if requires_flush {
                        debug!("🌀 Flushing batch for session {}", session_id_hex);
                        writer.write_stream.flush().await
                            .map_err(BatchError::Io)?;
                        debug!("✅ Batch flushed for session {}", session_id_hex);
                    }

                    debug!("✅ Batch write to {} session {}: {} tasks, {} bytes",
                        destination_addr, session_id_hex, tasks.len(), total_bytes);

                    Ok(())
                }
                Ok(Err(e)) => {
                    writer.is_active = false;
                    error!("❌ Batch write failed for session {}: {}",
                        session_id_hex, e);
                    Err(BatchError::ProcessingError(e.to_string()))
                }
                Err(_) => {
                    writer.is_active = false;
                    error!("⏰ Batch write timeout for session {}",
                        session_id_hex);
                    Err(BatchError::Timeout)
                }
            }
        } else {
            error!("Connection not found for batch write to {}",
                destination_addr);
            Err(BatchError::ConnectionError("Connection not found".to_string()))
        }
    }

    pub async fn shutdown(&self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);

        let mut connections = self.connections.write().await;
        for connection in connections.iter_mut() {
            connection.is_active = false;
        }

        info!("BatchWriter shutdown completed with {} connections",
            connections.len());
    }
}