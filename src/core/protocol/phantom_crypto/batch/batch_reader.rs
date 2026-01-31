use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::{HashMap, VecDeque};
use tokio::io::AsyncRead;
use tokio::sync::{mpsc, RwLock, Mutex};
use tokio::time::timeout;
use tracing::{info, debug, warn, error};
use bytes::BytesMut;

use crate::core::protocol::packets::frame_reader;

/// Конфигурация пакетного чтения
#[derive(Debug, Clone)]
pub struct BatchReaderConfig {
    pub batch_size: usize,           // Оптимальный размер батча
    pub buffer_size: usize,          // Размер буфера чтения
    pub read_timeout: Duration,      // Таймаут на чтение
    pub max_pending_batches: usize,  // Максимальное количество ожидающих батчей
    pub enable_adaptive_batching: bool, // Адаптивный размер батча
    pub min_batch_size: usize,       // Минимальный размер батча
    pub max_batch_size: usize,       // Максимальный размер батча
}

impl Default for BatchReaderConfig {
    fn default() -> Self {
        Self {
            batch_size: 64,
            buffer_size: 65536,      // 64KB
            read_timeout: Duration::from_secs(30),
            max_pending_batches: 100,
            enable_adaptive_batching: true,
            min_batch_size: 8,
            max_batch_size: 256,
        }
    }
}

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

/// Статистика читателя
#[derive(Debug, Clone, Default)]
pub struct ReaderStats {
    pub total_frames_read: u64,
    pub total_bytes_read: u64,
    pub total_batches_processed: u64,
    pub avg_batch_size: f64,
    pub avg_frame_size: f64,
    pub read_timeouts: u64,
    pub read_errors: u64,
    pub current_pending_batches: usize,
    pub frames_per_second: f64,
    pub bytes_per_second: f64,
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

/// Читатель для конкретного соединения
struct ConnectionReader {
    source_addr: std::net::SocketAddr,
    session_id: Vec<u8>,
    read_stream: Box<dyn AsyncRead + Unpin + Send + Sync>,
    buffer: BytesMut,
    last_read_time: Instant,
    frames_read: u64,
    is_active: bool,
}

/// Адаптивный тюнер размера батча
#[derive(Debug, Clone)]
struct AdaptiveBatchTuner {
    current_batch_size: usize,
    min_batch_size: usize,
    max_batch_size: usize,
    history: VecDeque<BatchPerformance>,
    learning_rate: f64,
    target_latency: Duration,
}

#[derive(Debug, Clone)]
struct BatchPerformance {
    batch_size: usize,
    processing_time: Duration,
    frames_per_second: f64,
    timestamp: Instant,
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
    ) -> Result<(), BatchReaderError> {
        let mut connections = self.active_connections.write().await;

        if connections.contains_key(&source_addr) {
            return Err(BatchReaderError::ConnectionAlreadyRegistered);
        }

        let connection_reader = ConnectionReader {
            source_addr,
            session_id: session_id.clone(),
            read_stream,
            buffer: BytesMut::with_capacity(self.config.buffer_size),
            last_read_time: Instant::now(),
            frames_read: 0,
            is_active: true,
        };

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
    ) -> Result<Option<BatchFrame>, BatchReaderError> {
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
                        return Err(BatchReaderError::ConnectionClosed);
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
                    Err(BatchReaderError::FrameReadError(e.to_string()))
                }
            },
            Err(_) => {
                // Таймаут чтения
                let mut stats = self.stats.lock().await;
                stats.read_timeouts += 1;

                Err(BatchReaderError::ReadTimeout)
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
    async fn handle_read_error(&self, source_addr: std::net::SocketAddr, error: BatchReaderError) {
        let error_msg = match error {
            BatchReaderError::ConnectionClosed => "Connection closed by peer".to_string(),
            BatchReaderError::ReadTimeout => "Read timeout".to_string(),
            BatchReaderError::FrameReadError(e) => format!("Frame read error: {}", e),
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

impl Clone for ConnectionReader {
    fn clone(&self) -> Self {
        // Используем tokio::io::empty() вместо std::io::empty()
        Self {
            source_addr: self.source_addr,
            session_id: self.session_id.clone(),
            read_stream: Box::new(tokio::io::empty()),
            buffer: BytesMut::with_capacity(self.buffer.capacity()),
            last_read_time: self.last_read_time,
            frames_read: self.frames_read,
            is_active: self.is_active,
        }
    }
}

/// Адаптивный тюнер размера батча
impl AdaptiveBatchTuner {
    fn new(
        initial_size: usize,
        min_size: usize,
        max_size: usize,
        target_latency: Duration,
    ) -> Self {
        Self {
            current_batch_size: initial_size,
            min_batch_size: min_size,
            max_batch_size: max_size,
            history: VecDeque::with_capacity(100),
            learning_rate: 0.1,
            target_latency,
        }
    }

    /// Настройка размера батча на основе производительности
    fn adjust_batch_size(&mut self, actual_batch_size: usize, processing_time: Duration) -> usize {
        // Сохраняем историю производительности
        let performance = BatchPerformance {
            batch_size: actual_batch_size,
            processing_time,
            frames_per_second: actual_batch_size as f64 / processing_time.as_secs_f64(),
            timestamp: Instant::now(),
        };

        self.history.push_back(performance);

        // Ограничиваем историю
        if self.history.len() > 100 {
            self.history.pop_front();
        }

        // Анализируем последние N батчей
        let recent_history: Vec<_> = self.history.iter().rev().take(10).collect();

        if recent_history.is_empty() {
            return self.current_batch_size;
        }

        // Рассчитываем среднюю производительность
        let avg_fps: f64 = recent_history.iter()
            .map(|p| p.frames_per_second)
            .sum::<f64>() / recent_history.len() as f64;

        let avg_latency: Duration = recent_history.iter()
            .map(|p| p.processing_time)
            .sum::<Duration>() / recent_history.len() as u32;

        // Адаптивная настройка
        if avg_latency > self.target_latency * 2 {
            // Слишком большая задержка - уменьшаем размер батча
            self.current_batch_size = (self.current_batch_size as f64 * 0.8)
                .max(self.min_batch_size as f64) as usize;
        } else if avg_latency < self.target_latency / 2 && avg_fps > 1000.0 {
            // Хорошая производительность, можно увеличить
            self.current_batch_size = (self.current_batch_size as f64 * 1.2)
                .min(self.max_batch_size as f64) as usize;
        }

        debug!("Adaptive batch tuning: size={}, latency={:?}, fps={:.1}",
               self.current_batch_size, avg_latency, avg_fps);

        self.current_batch_size
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BatchReaderError {
    #[error("Connection already registered")]
    ConnectionAlreadyRegistered,
    #[error("Connection closed")]
    ConnectionClosed,
    #[error("Read timeout")]
    ReadTimeout,
    #[error("Frame read error: {0}")]
    FrameReadError(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}