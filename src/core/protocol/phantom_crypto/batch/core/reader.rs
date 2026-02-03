use std::sync::Arc;
use std::time::{Instant, Duration};
use tokio::io::AsyncRead;
use tokio::sync::{mpsc, Mutex};
use bytes::BytesMut;
use tracing::{info, debug, error};

use crate::core::protocol::packets::frame_reader;

use crate::core::protocol::phantom_crypto::batch::config::BatchConfig;
use crate::core::protocol::phantom_crypto::batch::types::error::BatchError;
use crate::core::protocol::phantom_crypto::batch::types::priority::Priority;

/// Событие от читателя
#[derive(Debug)]
pub enum ReaderEvent {
    DataReady {
        session_id: Vec<u8>,
        data: BytesMut,
        source_addr: std::net::SocketAddr,
        priority: Priority,
        received_at: Instant,
    },
    ConnectionClosed {
        source_addr: std::net::SocketAddr,
        reason: String,
    },
    Error {
        source_addr: std::net::SocketAddr,
        error: BatchError,
    },
}

/// Читатель данных
pub struct BatchReader {
    config: BatchConfig,
    event_tx: mpsc::Sender<ReaderEvent>,
    is_running: Arc<std::sync::atomic::AtomicBool>,
}

impl BatchReader {
    pub fn new(config: BatchConfig, event_tx: mpsc::Sender<ReaderEvent>) -> Self {
        Self {
            config,
            event_tx,
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    pub async fn register_connection(
        &self,
        source_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        read_stream: Box<dyn AsyncRead + Unpin + Send + Sync>,
    ) -> Result<(), BatchError> {
        info!("Starting reader for connection: {} session: {}",
            source_addr, hex::encode(&session_id));

        let event_tx = self.event_tx.clone();
        let config = self.config.clone();
        let is_running = self.is_running.clone();

        // Клонируем session_id для использования в замыкании
        let session_id_clone = session_id.clone();

        tokio::spawn(async move {
            info!("Reader task started for {}", source_addr);

            // Обернем read_stream в Mutex для потокобезопасного доступа
            let read_stream = Arc::new(Mutex::new(read_stream));
            let session_id_inner = session_id_clone.clone();

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                let read_result = {
                    let mut stream_guard = read_stream.lock().await;
                    // Используем dyn dispatch для работы с трейт-объектом
                    Self::read_from_stream_dyn(&mut **stream_guard, &config).await
                };

                match read_result {
                    Ok(Some((data, bytes_read))) => {
                        let priority = Priority::from_byte(&data);

                        let event = ReaderEvent::DataReady {
                            session_id: session_id_inner.clone(),
                            data,
                            source_addr,
                            priority,
                            received_at: Instant::now(),
                        };

                        if let Err(e) = event_tx.send(event).await {
                            error!("Failed to send reader event for {}: {}", source_addr, e);
                            break;
                        }

                        debug!("Read {} bytes from {}", bytes_read, source_addr);
                    }
                    Ok(None) => {
                        // Нет данных, продолжаем
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(e) => {
                        error!("Read error from {}: {}", source_addr, e);

                        let event = ReaderEvent::Error {
                            source_addr,
                            error: e,
                        };

                        event_tx.send(event).await.ok();
                        break;
                    }
                }

                // Краткая пауза для предотвращения busy loop
                tokio::time::sleep(Duration::from_millis(1)).await;
            }

            info!("Reader task finished for {}", source_addr);

            // Отправляем событие о закрытии соединения
            let event = ReaderEvent::ConnectionClosed {
                source_addr,
                reason: "Reader task finished".to_string(),
            };
            event_tx.send(event).await.ok();
        });

        info!("✅ Reader registered for connection: {} session: {}",
            source_addr, hex::encode(&session_id));

        Ok(())
    }

    // Отдельный метод для работы с dyn трейт-объектами
    async fn read_from_stream_dyn(
        read_stream: &mut (dyn AsyncRead + Unpin + Send + Sync),
        config: &BatchConfig,
    ) -> Result<Option<(BytesMut, usize)>, BatchError> {
        debug!("Attempting to read frame from stream...");
        let mut buffer = BytesMut::with_capacity(config.read_buffer_size);

        // Используем frame_reader для чтения
        match tokio::time::timeout(
            config.read_timeout,
            frame_reader::read_frame(read_stream),
        ).await {
            Ok(Ok(data)) if !data.is_empty() => {
                debug!("Read frame: {} bytes, first 8 bytes: {:?}",
               data.len(),
               &data[..std::cmp::min(8, data.len())]);
                let bytes_read = data.len();
                buffer.extend_from_slice(&data);
                Ok(Some((buffer, bytes_read)))
            }
            Ok(Ok(_)) => {
                // Пустые данные - соединение закрыто
                Ok(None)
            }
            Ok(Err(e)) => {
                Err(BatchError::ProcessingError(e.to_string()))
            }
            Err(_) => {
                Err(BatchError::Timeout)
            }
        }
    }

    pub async fn shutdown(&self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
        info!("BatchReader shutdown initiated");
    }
}