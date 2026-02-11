use std::sync::Arc;
use std::time::{Instant, Duration};
use tokio::io::AsyncRead;
use tokio::sync::{mpsc, Mutex};
use bytes::BytesMut;
use tracing::{info, debug, error, warn};

use crate::core::protocol::packets::frame_reader;
use crate::core::protocol::batch_system::config::BatchConfig;
use crate::core::protocol::batch_system::types::error::BatchError;
use crate::core::protocol::batch_system::types::priority::Priority;

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
        if self.event_tx.is_closed() {
            error!("❌ Event channel is closed, cannot register connection");
            return Err(BatchError::ConnectionError("Event channel closed".to_string()));
        }

        let event_tx = self.event_tx.clone();
        let config = self.config.clone();
        let is_running = self.is_running.clone();
        let session_id_clone = session_id.clone();

        tokio::spawn(async move {
            let read_stream = Arc::new(Mutex::new(read_stream));
            let session_id_inner = session_id_clone.clone();
            let mut consecutive_read_errors = 0;
            const MAX_CONSECUTIVE_ERRORS: u32 = 3;

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                let read_result = {
                    let mut stream_guard = read_stream.lock().await;
                    Self::read_from_stream_dyn(&mut **stream_guard, &config).await
                };

                match read_result {
                    Ok(Some((data, _bytes_read))) => {
                        consecutive_read_errors = 0;

                        let priority = Priority::from_byte(&data);

                        let event = ReaderEvent::DataReady {
                            session_id: session_id_inner.clone(),
                            data,
                            source_addr,
                            priority,
                            received_at: Instant::now(),
                        };

                        if let Err(e) = event_tx.send(event).await {
                            error!("❌ Failed to send reader event for {}: {}", source_addr, e);
                            break;
                        }
                    }
                    Ok(None) => {
                        debug!("🔌 Connection closed gracefully for {}", source_addr);
                        break;
                    }
                    Err(e) => {
                        consecutive_read_errors += 1;

                        if consecutive_read_errors >= MAX_CONSECUTIVE_ERRORS {
                            error!("❌ Too many consecutive read errors ({}) for {}: {}",
                                consecutive_read_errors, source_addr, e);
                            break;
                        }

                        match &e {
                            BatchError::ConnectionClosed(reason) => {
                                debug!("🔌 Connection closed for {}: {}", source_addr, reason);
                                break;
                            }
                            _ => {
                                warn!("⚠️ Read error from {} (attempt {}): {}",
                                    source_addr, consecutive_read_errors, e);
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        }
                    }
                }

                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            info!("📕 Reader task finished for {}", source_addr);

            let event = ReaderEvent::ConnectionClosed {
                source_addr,
                reason: "Reader task finished".to_string(),
            };

            if let Err(e) = event_tx.send(event).await {
                debug!("Failed to send connection closed event: {}", e);
            }
        });

        Ok(())
    }

    async fn read_from_stream_dyn(
        read_stream: &mut (dyn AsyncRead + Unpin + Send + Sync),
        config: &BatchConfig,
    ) -> Result<Option<(BytesMut, usize)>, BatchError> {
        match tokio::time::timeout(
            config.read_timeout,
            frame_reader::read_frame(read_stream),
        ).await {
            Ok(Ok(data)) => {
                if data.is_empty() {
                    return Ok(None);
                }

                let bytes_read = data.len();
                let mut buffer = BytesMut::with_capacity(bytes_read);
                buffer.extend_from_slice(&data);
                Ok(Some((buffer, bytes_read)))
            }
            Ok(Err(e)) => {
                if e.to_string().contains("Connection closed") ||
                    e.to_string().contains("UnexpectedEof") ||
                    e.to_string().contains("Broken pipe") ||
                    e.to_string().contains("Connection reset") {
                    return Err(BatchError::ConnectionClosed(e.to_string()));
                }
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