use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::HashMap;
use tokio::io::AsyncRead;
use tokio::sync::{mpsc, RwLock, Mutex};
use tokio::time::timeout;
use tracing::{info, debug, warn, error};
use bytes::BytesMut;
use crate::core::protocol::error::ProtocolError;
use crate::core::protocol::packets::frame_reader;
pub(crate) use super::config::BatchReaderConfig;
use super::stats::ReaderStats;
use super::connection_reader::ConnectionReader;
use crate::core::protocol::phantom_crypto::batch::buffer::adaptive_tuner::AdaptiveBatchTuner;
use crate::core::protocol::phantom_crypto::batch::types::error::BatchReaderError;

/// Событие от пакетного читателя
#[derive(Debug)]
pub enum BatchReaderEvent {
    BatchReady {
        batch_id: u64,
        frames: Vec<BatchFrame>,
        source_addr: std::net::SocketAddr,
        received_at: Instant,
    },
    ConnectionClosed {
        source_addr: std::net::SocketAddr,
        reason: String,
    },
    ReadError {
        source_addr: std::net::SocketAddr,
        error: String,
    },
    StatisticsUpdate {
        stats: ReaderStats,
    },
}

/// Фрейм в батче
#[derive(Debug, Clone)]
pub struct BatchFrame {
    pub session_id: Vec<u8>,         // Идентификатор сессии
    pub data: BytesMut,              // Данные фрейма
    pub received_at: Instant,        // Время получения
    pub frame_size: usize,           // Размер фрейма
    pub priority: FramePriority,     // Приоритет фрейма
}

/// Приоритет фрейма для диспетчеризации
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FramePriority {
    Critical = 0,    // Heartbeat, управляющие команды
    High = 1,        // Важные данные
    Normal = 2,      // Обычный трафик
    Low = 3,         // Фоновые задачи
}

/// Пакетный читатель
pub struct BatchReader {
    config: BatchReaderConfig,
    event_tx: mpsc::Sender<BatchReaderEvent>,
    active_connections: Arc<RwLock<HashMap<std::net::SocketAddr, ConnectionReader>>>,
    stats: Arc<Mutex<ReaderStats>>,
    batch_counter: Arc<std::sync::atomic::AtomicU64>,  // Изменено на Arc<AtomicU64>
    adaptive_tuner: Arc<Mutex<AdaptiveBatchTuner>>,
}

impl BatchReader {
    /// Создание нового пакетного читателя
    pub fn new(
        config: BatchReaderConfig,
        event_tx: mpsc::Sender<BatchReaderEvent>,
    ) -> Self {
        let adaptive_tuner = AdaptiveBatchTuner::new(
            config.batch_size,
            config.min_batch_size,
            config.max_batch_size,
            Duration::from_millis(10),
        );

        Self {
            config,
            event_tx,
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(Mutex::new(ReaderStats::default())),
            batch_counter: Arc::new(std::sync::atomic::AtomicU64::new(0)),  // Arc::new
            adaptive_tuner: Arc::new(Mutex::new(adaptive_tuner)),
        }
    }

    /// Регистрация нового соединения для пакетного чтения
    pub async fn register_connection(
        &self,
        source_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        read_stream: Box<dyn AsyncRead + Unpin + Send + Sync>,
    ) -> Result<(), crate::core::protocol::phantom_crypto::batch::types::error::BatchReaderError> {
        let mut connections = self.active_connections.write().await;

        if connections.contains_key(&source_addr) {
            return Err(crate::core::protocol::phantom_crypto::batch::types::error::BatchReaderError::ConnectionAlreadyRegistered);
        }

        let connection_reader = ConnectionReader::new(
            source_addr,
            session_id.clone(),
            read_stream,
            self.config.buffer_size,
        );

        connections.insert(source_addr, connection_reader.clone());

        // Запускаем обработчик для этого соединения с передачей Arc-ссылок
        self.spawn_connection_handler(
            source_addr,
            self.active_connections.clone(),
            self.stats.clone(),
            self.event_tx.clone(),
            self.adaptive_tuner.clone(),
            self.config.clone(),
            self.batch_counter.clone(),  // Теперь можно клонировать Arc
        ).await;

        info!("📥 BatchReader registered connection: {} session: {}",
              source_addr, hex::encode(&session_id));

        Ok(())
    }

    /// Запуск обработчика соединения с передачей Arc-ссылок
    async fn spawn_connection_handler(
        &self,
        source_addr: std::net::SocketAddr,
        active_connections: Arc<RwLock<HashMap<std::net::SocketAddr, ConnectionReader>>>,
        stats: Arc<Mutex<ReaderStats>>,
        event_tx: mpsc::Sender<BatchReaderEvent>,
        adaptive_tuner: Arc<Mutex<AdaptiveBatchTuner>>,
        config: BatchReaderConfig,
        batch_counter: Arc<std::sync::atomic::AtomicU64>,  // Изменен тип
    ) {
        tokio::spawn(async move {
            BatchReader::handle_connection_internal(
                source_addr,
                active_connections,
                stats,
                event_tx,
                adaptive_tuner,
                config,
                batch_counter,
            ).await;
        });
    }

    /// Внутренний обработчик соединения (static метод)
    async fn handle_connection_internal(
        source_addr: std::net::SocketAddr,
        active_connections: Arc<RwLock<HashMap<std::net::SocketAddr, ConnectionReader>>>,
        stats: Arc<Mutex<ReaderStats>>,
        event_tx: mpsc::Sender<BatchReaderEvent>,
        adaptive_tuner: Arc<Mutex<AdaptiveBatchTuner>>,
        config: BatchReaderConfig,
        batch_counter: Arc<std::sync::atomic::AtomicU64>,  // Изменен тип
    ) {
        // Получаем соединение из HashMap
        let connection_opt = {
            let connections = active_connections.read().await;
            connections.get(&source_addr).cloned()
        };

        if connection_opt.is_none() {
            warn!("Connection not found for {}", source_addr);
            return;
        }

        let mut connection = connection_opt.unwrap();
        let mut batch_frames = Vec::with_capacity(config.batch_size);
        let mut current_batch_size = config.batch_size;

        info!("🔄 BatchReader started for {}", source_addr);

        while connection.is_active {
            let batch_start = Instant::now();

            // Собираем батч фреймов
            for _ in 0..current_batch_size {
                match BatchReader::read_single_frame_internal(&mut connection, &config).await {
                    Ok(Some(frame)) => {
                        // Обрабатываем фрейм...
                        let frame_size = frame.frame_size;
                        batch_frames.push(frame);

                        // Обновляем статистику
                        let mut stats_guard = stats.lock().await;
                        stats_guard.total_frames_read += 1;
                        stats_guard.total_bytes_read += frame_size as u64;
                    }
                    Ok(None) => {
                        // Нет данных (would block или таймаут)
                        // Ждем немного перед следующей попыткой
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        continue; // Продолжаем цикл
                    }
                    Err(e) => {
                        // Настоящая ошибка чтения
                        BatchReader::handle_read_error_internal(
                            source_addr,
                            e,
                            &event_tx,
                            &stats,
                        ).await;
                        connection.is_active = false;
                        break;
                    }
                }
            }

            if !batch_frames.is_empty() {
                // Отправляем готовый батч
                BatchReader::send_batch_ready_internal(
                    source_addr,
                    &mut batch_frames,
                    batch_start,
                    &event_tx,
                    &stats,
                    &batch_counter,
                ).await;

                // Адаптивная настройка размера батча
                if config.enable_adaptive_batching {
                    let mut tuner = adaptive_tuner.lock().await;
                    current_batch_size = tuner.adjust_batch_size(
                        batch_frames.len(),
                        batch_start.elapsed(),
                    );
                }
            }

            // Очищаем батч для следующей итерации
            batch_frames.clear();

            // Небольшая пауза для предотвращения busy loop
            tokio::time::sleep(Duration::from_micros(100)).await;
        }

        // Очистка соединения
        BatchReader::cleanup_connection_internal(
            source_addr,
            &active_connections,
            &event_tx,
        ).await;
    }

    /// Чтение одиночного фрейма (internal)
    async fn read_single_frame_internal(
        connection: &mut ConnectionReader,
        config: &BatchReaderConfig,
    ) -> Result<Option<BatchFrame>, BatchReaderError> {
        match timeout(
            config.read_timeout,
            frame_reader::read_frame(&mut connection.read_stream)
        ).await {
            Ok(Ok(data)) => {
                if data.is_empty() {
                    debug!("📭 EOF from {}", connection.source_addr);
                    return Ok(None);
                }

                let frame_size = data.len();
                connection.frames_read += 1;
                connection.last_read_time = Instant::now();

                debug!("📥 SUCCESS: Read {} bytes from {}", frame_size, connection.source_addr);

                let frame = BatchFrame {
                    session_id: connection.session_id.clone(),
                    data: BytesMut::from(&data[..]),
                    received_at: Instant::now(),
                    frame_size,
                    priority: BatchReader::determine_frame_priority_internal(&data),
                };

                Ok(Some(frame))
            }
            Ok(Err(e)) => {
                match &e {
                    ProtocolError::Timeout { .. } => {
                        debug!("⏰ Read timeout from {}", connection.source_addr);
                        Ok(None)
                    }
                    ProtocolError::Io(error_str) => {
                        if error_str.contains("WouldBlock") || error_str.contains("TimedOut") {
                            debug!("📭 Temporary IO issue from {}: {}", connection.source_addr, error_str);
                            Ok(None)
                        } else {
                            warn!("❌ IO error from {}: {}", connection.source_addr, error_str);
                            Err(BatchReaderError::FrameReadError(error_str.clone()))
                        }
                    }
                    _ => {
                        warn!("❌ Protocol error from {}: {}", connection.source_addr, e);
                        Err(BatchReaderError::FrameReadError(e.to_string()))
                    }
                }
            }
            Err(_) => {
                debug!("⏰ Read timeout from {} (no data available)", connection.source_addr);
                Ok(None)
            }
        }
    }

    /// Определение приоритета фрейма (internal)
    fn determine_frame_priority_internal(data: &[u8]) -> FramePriority {
        if data.is_empty() {
            return FramePriority::Normal;
        }

        // Heartbeat пакеты (0x10) - Critical
        if data[0] == 0x10 {
            return FramePriority::Critical;
        }

        // Маленькие пакеты (команды) - High
        if data.len() <= 64 {
            return FramePriority::High;
        }

        // Большие пакеты (данные) - Normal или Low
        if data.len() > 1024 {
            // Фоновые большие передачи
            FramePriority::Low
        } else {
            FramePriority::Normal
        }
    }

    /// Отправка готового батча (internal)
    async fn send_batch_ready_internal(
        source_addr: std::net::SocketAddr,
        frames: &mut Vec<BatchFrame>,
        batch_start: Instant,
        event_tx: &mpsc::Sender<BatchReaderEvent>,
        stats: &Arc<Mutex<ReaderStats>>,
        batch_counter: &Arc<std::sync::atomic::AtomicU64>,  // Изменен тип
    ) {
        let batch_id = batch_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Сортируем фреймы по приоритету
        frames.sort_by_key(|f| f.priority);

        let frames_len = frames.len();
        let batch_event = BatchReaderEvent::BatchReady {
            batch_id,
            frames: std::mem::take(frames),
            source_addr,
            received_at: batch_start,
        };

        if let Err(e) = event_tx.send(batch_event).await {
            error!("Failed to send batch ready event for {}: {}", source_addr, e);
        }

        // Обновляем статистику
        BatchReader::update_statistics_internal(frames_len, batch_start, stats, event_tx).await;
    }

    /// Обработка ошибки чтения (internal)
    async fn handle_read_error_internal(
        source_addr: std::net::SocketAddr,
        error: crate::core::protocol::phantom_crypto::batch::types::error::BatchReaderError,
        event_tx: &mpsc::Sender<BatchReaderEvent>,
        stats: &Arc<Mutex<ReaderStats>>,
    ) {
        let error_msg = match error {
            crate::core::protocol::phantom_crypto::batch::types::error::BatchReaderError::ConnectionClosed => "Connection closed by peer".to_string(),
            crate::core::protocol::phantom_crypto::batch::types::error::BatchReaderError::ReadTimeout => "Read timeout".to_string(),
            crate::core::protocol::phantom_crypto::batch::types::error::BatchReaderError::FrameReadError(e) => format!("Frame read error: {}", e),
            _ => "Unknown read error".to_string(),
        };

        let error_event = BatchReaderEvent::ReadError {
            source_addr,
            error: error_msg.clone(),
        };

        event_tx.send(error_event).await.ok();

        let mut stats_guard = stats.lock().await;
        stats_guard.read_errors += 1;

        warn!("❌ Read error for {}: {}", source_addr, error_msg);
    }

    /// Очистка соединения (internal)
    async fn cleanup_connection_internal(
        source_addr: std::net::SocketAddr,
        active_connections: &Arc<RwLock<HashMap<std::net::SocketAddr, ConnectionReader>>>,
        event_tx: &mpsc::Sender<BatchReaderEvent>,
    ) {
        let mut connections = active_connections.write().await;
        connections.remove(&source_addr);

        let close_event = BatchReaderEvent::ConnectionClosed {
            source_addr,
            reason: "Connection handler terminated".to_string(),
        };

        event_tx.send(close_event).await.ok();

        info!("📭 BatchReader connection closed: {}", source_addr);
    }

    /// Обновление статистики (internal)
    async fn update_statistics_internal(
        frames_in_batch: usize,
        batch_start: Instant,
        stats: &Arc<Mutex<ReaderStats>>,
        event_tx: &mpsc::Sender<BatchReaderEvent>,
    ) {
        let mut stats_guard = stats.lock().await;

        stats_guard.total_batches_processed += 1;

        // Обновляем средний размер батча
        let total_batches = stats_guard.total_batches_processed as f64;
        stats_guard.avg_batch_size =
            (stats_guard.avg_batch_size * (total_batches - 1.0) + frames_in_batch as f64) / total_batches;

        // Обновляем средний размер фрейма
        if stats_guard.total_frames_read > 0 {
            stats_guard.avg_frame_size = stats_guard.total_bytes_read as f64 / stats_guard.total_frames_read as f64;
        }

        // Расчет frames per second (скользящее среднее)
        let batch_time = batch_start.elapsed();
        if batch_time.as_micros() > 0 {
            let fps = frames_in_batch as f64 / (batch_time.as_micros() as f64 / 1_000_000.0);
            // Экспоненциальное скользящее среднее
            stats_guard.frames_per_second = 0.7 * stats_guard.frames_per_second + 0.3 * fps;
            stats_guard.bytes_per_second = stats_guard.frames_per_second * stats_guard.avg_frame_size;
        }

        // Отправляем обновление статистики
        let stats_event = BatchReaderEvent::StatisticsUpdate {
            stats: stats_guard.clone(),
        };

        event_tx.send(stats_event).await.ok();
    }

    /// Получение статистики
    pub async fn get_stats(&self) -> ReaderStats {
        self.stats.lock().await.clone()
    }

    /// Остановка всех читателей
    pub async fn shutdown(&self) {
        let mut connections = self.active_connections.write().await;

        // Деактивируем все соединения
        for connection in connections.values_mut() {
            connection.is_active = false;
        }

        info!("BatchReader shutdown initiated");
    }
}

impl Clone for BatchReader {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            event_tx: self.event_tx.clone(),
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(Mutex::new(ReaderStats::default())),
            batch_counter: Arc::new(std::sync::atomic::AtomicU64::new(0)),  // Новый AtomicU64
            adaptive_tuner: Arc::new(Mutex::new(AdaptiveBatchTuner::new(
                self.config.batch_size,
                self.config.min_batch_size,
                self.config.max_batch_size,
                Duration::from_millis(10),
            ))),
        }
    }
}