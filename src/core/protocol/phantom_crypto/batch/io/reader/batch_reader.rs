use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::{HashMap};
use tokio::io::AsyncRead;
use tokio::sync::{mpsc, RwLock, Mutex};
use tokio::time::timeout;
use tracing::{info, debug, warn, error};
use bytes::BytesMut;

pub(crate) use super::config::BatchReaderConfig;
use super::stats::ReaderStats;
use super::connection_reader::ConnectionReader;
use crate::core::protocol::phantom_crypto::batch::buffer::adaptive_tuner::AdaptiveBatchTuner;
use crate::core::protocol::packets::frame_reader;

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
    stats: Mutex<ReaderStats>,
    batch_counter: std::sync::atomic::AtomicU64,
    adaptive_tuner: Mutex<AdaptiveBatchTuner>,
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
            stats: Mutex::new(ReaderStats::default()),
            batch_counter: std::sync::atomic::AtomicU64::new(0),
            adaptive_tuner: Mutex::new(adaptive_tuner),
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

        connections.insert(source_addr, connection_reader);

        // Запускаем обработчик для этого соединения
        self.spawn_connection_handler(source_addr).await;

        info!("📥 BatchReader registered connection: {} session: {}",
              source_addr, hex::encode(&session_id));

        Ok(())
    }

    /// Запуск обработчика соединения
    async fn spawn_connection_handler(&self, source_addr: std::net::SocketAddr) {
        let batch_reader = self.clone();

        tokio::spawn(async move {
            batch_reader.handle_connection(source_addr).await;
        });
    }

    /// Обработка соединения
    async fn handle_connection(&self, source_addr: std::net::SocketAddr) {
        let connection_opt = {
            let connections = self.active_connections.read().await;
            connections.get(&source_addr).cloned()
        };

        if connection_opt.is_none() {
            warn!("Connection not found for {}", source_addr);
            return;
        }

        let mut connection = connection_opt.unwrap();
        let mut batch_frames = Vec::with_capacity(self.config.batch_size);
        let mut current_batch_size = self.config.batch_size;

        info!("🔄 BatchReader started for {}", source_addr);

        while connection.is_active {
            let batch_start = Instant::now();

            // Собираем батч фреймов
            for _ in 0..current_batch_size {
                match self.read_single_frame(&mut connection).await {
                    Ok(Some(frame)) => {
                        // Сохраняем размер фрейма для статистики
                        let frame_size = frame.frame_size;

                        batch_frames.push(frame);

                        // Обновляем статистику
                        let mut stats = self.stats.lock().await;
                        stats.total_frames_read += 1;
                        stats.total_bytes_read += frame_size as u64;
                    }
                    Ok(None) => {
                        // Нет данных (would block)
                        break;
                    }
                    Err(e) => {
                        // Ошибка чтения
                        self.handle_read_error(source_addr, e).await;
                        connection.is_active = false;
                        break;
                    }
                }
            }

            if !batch_frames.is_empty() {
                // Отправляем готовый батч
                self.send_batch_ready(source_addr, &mut batch_frames, batch_start).await;

                // Адаптивная настройка размера батча
                if self.config.enable_adaptive_batching {
                    let mut tuner = self.adaptive_tuner.lock().await;
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
        self.cleanup_connection(source_addr).await;
    }

    /// Чтение одиночного фрейма
    async fn read_single_frame(
        &self,
        connection: &mut ConnectionReader,
    ) -> Result<Option<BatchFrame>, crate::core::protocol::phantom_crypto::batch::types::error::BatchReaderError> {
        let start = Instant::now();

        // Используем frame_reader для чтения фрейма
        match timeout(
            self.config.read_timeout,
            frame_reader::read_frame(&mut connection.read_stream)
        ).await {
            Ok(read_result) => match read_result {
                Ok(data) => {
                    if data.is_empty() {
                        // Соединение закрыто
                        return Err(crate::core::protocol::phantom_crypto::batch::types::error::BatchReaderError::ConnectionClosed);
                    }

                    let frame_size = data.len();
                    connection.frames_read += 1;
                    connection.last_read_time = Instant::now();

                    // Определяем приоритет фрейма
                    let priority = self.determine_frame_priority(&data);

                    // Создаем BatchFrame
                    let frame = BatchFrame {
                        session_id: connection.session_id.clone(),
                        data: BytesMut::from(&data[..]),
                        received_at: Instant::now(),
                        frame_size,
                        priority,
                    };

                    debug!("📥 Read frame from {}: {} bytes, priority: {:?}, time: {:?}",
                           connection.source_addr, frame_size, priority, start.elapsed());

                    Ok(Some(frame))
                }
                Err(e) => {
                    // Ошибка чтения фрейма
                    Err(crate::core::protocol::phantom_crypto::batch::types::error::BatchReaderError::FrameReadError(e.to_string()))
                }
            },
            Err(_) => {
                // Таймаут чтения
                let mut stats = self.stats.lock().await;
                stats.read_timeouts += 1;

                Err(crate::core::protocol::phantom_crypto::batch::types::error::BatchReaderError::ReadTimeout)
            }
        }
    }

    /// Определение приоритета фрейма
    fn determine_frame_priority(&self, data: &[u8]) -> FramePriority {
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

    /// Отправка готового батча
    async fn send_batch_ready(
        &self,
        source_addr: std::net::SocketAddr,
        frames: &mut Vec<BatchFrame>,
        batch_start: Instant,
    ) {
        let batch_id = self.batch_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Сортируем фреймы по приоритету
        frames.sort_by_key(|f| f.priority);

        let frames_len = frames.len();
        let batch_event = BatchReaderEvent::BatchReady {
            batch_id,
            frames: std::mem::take(frames),
            source_addr,
            received_at: batch_start,
        };

        if let Err(e) = self.event_tx.send(batch_event).await {
            error!("Failed to send batch ready event for {}: {}", source_addr, e);
        }

        // Обновляем статистику
        self.update_statistics(frames_len, batch_start).await;
    }

    /// Обработка ошибки чтения
    async fn handle_read_error(&self, source_addr: std::net::SocketAddr, error: crate::core::protocol::phantom_crypto::batch::types::error::BatchReaderError) {
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

        self.event_tx.send(error_event).await.ok();

        let mut stats = self.stats.lock().await;
        stats.read_errors += 1;

        warn!("❌ Read error for {}: {}", source_addr, error_msg);
    }

    /// Очистка соединения
    async fn cleanup_connection(&self, source_addr: std::net::SocketAddr) {
        let mut connections = self.active_connections.write().await;
        connections.remove(&source_addr);

        let close_event = BatchReaderEvent::ConnectionClosed {
            source_addr,
            reason: "Connection handler terminated".to_string(),
        };

        self.event_tx.send(close_event).await.ok();

        info!("📭 BatchReader connection closed: {}", source_addr);
    }

    /// Обновление статистики
    async fn update_statistics(&self, frames_in_batch: usize, batch_start: Instant) {
        let mut stats = self.stats.lock().await;

        stats.total_batches_processed += 1;

        // Обновляем средний размер батча
        let total_batches = stats.total_batches_processed as f64;
        stats.avg_batch_size =
            (stats.avg_batch_size * (total_batches - 1.0) + frames_in_batch as f64) / total_batches;

        // Обновляем средний размер фрейма
        if stats.total_frames_read > 0 {
            stats.avg_frame_size = stats.total_bytes_read as f64 / stats.total_frames_read as f64;
        }

        // Расчет frames per second (скользящее среднее)
        let batch_time = batch_start.elapsed();
        if batch_time.as_micros() > 0 {
            let fps = frames_in_batch as f64 / (batch_time.as_micros() as f64 / 1_000_000.0);
            // Экспоненциальное скользящее среднее
            stats.frames_per_second = 0.7 * stats.frames_per_second + 0.3 * fps;
            stats.bytes_per_second = stats.frames_per_second * stats.avg_frame_size;
        }

        // Отправляем обновление статистики
        let stats_event = BatchReaderEvent::StatisticsUpdate {
            stats: stats.clone(),
        };

        self.event_tx.send(stats_event).await.ok();
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
            stats: Mutex::new(ReaderStats::default()),
            batch_counter: std::sync::atomic::AtomicU64::new(0),
            adaptive_tuner: Mutex::new(AdaptiveBatchTuner::new(
                self.config.batch_size,
                self.config.min_batch_size,
                self.config.max_batch_size,
                Duration::from_millis(10),
            )),
        }
    }
}