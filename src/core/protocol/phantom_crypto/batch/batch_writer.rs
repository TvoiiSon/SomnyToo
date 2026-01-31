use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::{HashMap, VecDeque};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, RwLock, Mutex, Semaphore};
use tokio::time::{timeout, interval};
use tracing::{info, debug, error};
use bytes::{Bytes, BytesMut};

use crate::core::protocol::packets::frame_writer;

/// Конфигурация пакетной записи
#[derive(Debug, Clone)]
pub struct BatchWriterConfig {
    pub batch_size: usize,           // Размер батча для записи
    pub buffer_size: usize,          // Размер буфера записи
    pub write_timeout: Duration,     // Таймаут на запись
    pub flush_interval: Duration,    // Интервал автоматического сброса
    pub max_pending_writes: usize,   // Максимальное количество ожидающих записей
    pub enable_buffering: bool,      // Включить буферизацию
    pub max_buffer_size: usize,      // Максимальный размер буфера
    pub enable_nagle: bool,          // Алгоритм Nagle для TCP
}

impl Default for BatchWriterConfig {
    fn default() -> Self {
        Self {
            batch_size: 64,
            buffer_size: 65536,      // 64KB
            write_timeout: Duration::from_secs(5),
            flush_interval: Duration::from_millis(100),
            max_pending_writes: 1000,
            enable_buffering: true,
            max_buffer_size: 1024 * 1024, // 1MB
            enable_nagle: false,
        }
    }
}

/// Задача записи
#[derive(Debug, Clone)]
pub struct WriteTask {
    pub destination_addr: std::net::SocketAddr,
    pub session_id: Vec<u8>,
    pub data: Bytes,
    pub priority: WritePriority,
    pub created_at: Instant,
    pub requires_flush: bool,
}

/// Приоритет записи
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WritePriority {
    Immediate = 0,   // Немедленная запись (heartbeat, ACK)
    High = 1,        // Высокий приоритет
    Normal = 2,      // Обычный приоритет
    Low = 3,         // Низкий приоритет (фоновые задачи)
}

/// Событие от пакетного писателя
#[derive(Debug)]
pub enum BatchWriterEvent {
    WriteCompleted {
        destination_addr: std::net::SocketAddr,
        batch_id: u64,
        bytes_written: usize,
        write_time: Duration,
    },
    WriteError {
        destination_addr: std::net::SocketAddr,
        error: String,
    },
    BufferFull {
        destination_addr: std::net::SocketAddr,
        buffer_size: usize,
    },
    StatisticsUpdate {
        stats: WriterStats,
    },
}

/// Статистика писателя
#[derive(Debug, Clone, Default)]
pub struct WriterStats {
    pub total_writes: u64,
    pub total_bytes_written: u64,
    pub total_batches_written: u64,
    pub avg_batch_size: f64,
    pub avg_write_time: Duration,
    pub write_timeouts: u64,
    pub write_errors: u64,
    pub buffer_hits: u64,           // Буферизованные записи
    pub immediate_writes: u64,      // Немедленные записи
    pub current_buffer_size: usize,
    pub writes_per_second: f64,
    pub bytes_per_second: f64,
}

/// Пакетный писатель
pub struct BatchWriter {
    config: BatchWriterConfig,
    event_tx: mpsc::Sender<BatchWriterEvent>,
    active_writers: Arc<RwLock<HashMap<std::net::SocketAddr, ConnectionWriter>>>,
    write_queue: mpsc::Sender<WriteTask>,
    write_queue_rx: Mutex<mpsc::Receiver<WriteTask>>,
    stats: Mutex<WriterStats>,
    batch_counter: std::sync::atomic::AtomicU64,
    flush_timer: Mutex<tokio::time::Interval>,
    backpressure_semaphore: Arc<Semaphore>,
}

/// Писатель для конкретного соединения
struct ConnectionWriter {
    destination_addr: std::net::SocketAddr,
    session_id: Vec<u8>,
    write_stream: Box<dyn AsyncWrite + Unpin + Send + Sync>,
    write_buffer: BytesMut,
    last_write_time: Instant,
    bytes_written: u64,
    is_active: bool,
    pending_writes: VecDeque<WriteTask>,
    buffer_size: usize,
}

impl BatchWriter {
    /// Создание нового пакетного писателя
    pub fn new(
        config: BatchWriterConfig,
        event_tx: mpsc::Sender<BatchWriterEvent>,
    ) -> Self {
        let (write_tx, write_rx) = mpsc::channel(config.max_pending_writes);
        let mut flush_timer = interval(config.flush_interval);
        flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let writer = Self {
            config: config.clone(),
            event_tx,
            active_writers: Arc::new(RwLock::new(HashMap::new())),
            write_queue: write_tx,
            write_queue_rx: Mutex::new(write_rx),
            stats: Mutex::new(WriterStats::default()),
            batch_counter: std::sync::atomic::AtomicU64::new(0),
            flush_timer: Mutex::new(flush_timer),
            backpressure_semaphore: Arc::new(Semaphore::new(config.max_pending_writes)),
        };

        // Запускаем обработчик очереди записи
        writer.start_queue_handler();

        // Запускаем таймер сброса
        writer.start_flush_timer();

        info!("🚀 BatchWriter initialized with batch size: {}", config.batch_size);

        writer
    }

    /// Регистрация нового соединения для пакетной записи
    pub async fn register_connection(
        &self,
        destination_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        write_stream: Box<dyn AsyncWrite + Unpin + Send + Sync>,
    ) -> Result<(), BatchWriterError> {
        let mut writers = self.active_writers.write().await;

        if writers.contains_key(&destination_addr) {
            return Err(BatchWriterError::ConnectionAlreadyRegistered);
        }

        let connection_writer = ConnectionWriter {
            destination_addr,
            session_id: session_id.clone(),
            write_stream,
            write_buffer: BytesMut::with_capacity(self.config.buffer_size),
            last_write_time: Instant::now(),
            bytes_written: 0,
            is_active: true,
            pending_writes: VecDeque::new(),
            buffer_size: 0,
        };

        writers.insert(destination_addr, connection_writer);

        info!("📤 BatchWriter registered connection: {} session: {}",
              destination_addr, hex::encode(&session_id));

        Ok(())
    }

    /// Постановка задачи записи в очередь
    pub async fn queue_write(
        &self,
        destination_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        data: Bytes,
        priority: WritePriority,
        requires_flush: bool,
    ) -> Result<(), BatchWriterError> {
        let start = Instant::now();

        // Проверяем backpressure
        let permit = self.backpressure_semaphore.clone()
            .try_acquire_owned()
            .map_err(|_| BatchWriterError::Backpressure)?;

        let write_task = WriteTask {
            destination_addr,
            session_id,
            data: data.clone(),
            priority,
            created_at: Instant::now(),
            requires_flush,
        };

        // В зависимости от приоритета выбираем стратегию
        let result = match priority {
            WritePriority::Immediate => {
                // Немедленная запись
                let task_clone = write_task.clone();
                let write_result = self.write_immediate(task_clone).await?;

                let mut stats = self.stats.lock().await;
                stats.immediate_writes += 1;

                Ok(write_result)
            }
            _ => {
                // Буферизованная запись
                if let Err(e) = self.write_queue.send(write_task.clone()).await {
                    drop(permit); // Освобождаем permit при ошибке
                    return Err(BatchWriterError::QueueError(e.to_string()));
                }

                let mut stats = self.stats.lock().await;
                stats.buffer_hits += 1;

                Ok(())
            }
        };

        debug!("📤 Write queued for {}: {} bytes, priority: {:?}, time: {:?}",
               destination_addr, write_task.data.len(), priority, start.elapsed());

        // Возвращаем результат с Ok(()) если все хорошо
        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Немедленная запись (без буферизации)
    async fn write_immediate(&self, task: WriteTask) -> Result<(), BatchWriterError> {
        let start = Instant::now();

        let mut writers = self.active_writers.write().await;
        let writer_opt = writers.get_mut(&task.destination_addr);

        if let Some(writer) = writer_opt {
            let mut write_stream = &mut *writer.write_stream;

            match timeout(
                self.config.write_timeout,
                self.write_single_frame(&task, &mut write_stream)
            ).await {
                Ok(write_result) => match write_result {
                    Ok(bytes_written) => {
                        // Отправляем событие о завершении записи
                        self.send_write_completed(
                            task.destination_addr,
                            bytes_written,
                            start,
                        ).await;

                        Ok(())
                    }
                    Err(e) => {
                        self.send_write_error(task.destination_addr, e.to_string()).await;
                        Err(BatchWriterError::WriteError(e.to_string()))
                    }
                },
                Err(_) => {
                    // Таймаут записи
                    let mut stats = self.stats.lock().await;
                    stats.write_timeouts += 1;

                    self.send_write_error(task.destination_addr, "Write timeout".to_string()).await;
                    Err(BatchWriterError::WriteError("Write timeout".to_string()))
                }
            }
        } else {
            Err(BatchWriterError::ConnectionNotFound)
        }
    }

    /// Запись одиночного фрейма
    async fn write_single_frame(
        &self,
        task: &WriteTask,
        write_stream: &mut (impl AsyncWrite + Unpin + Send + Sync),
    ) -> Result<usize, BatchWriterError> {
        // Используем frame_writer для записи фрейма
        frame_writer::write_frame(write_stream, &task.data)
            .await
            .map_err(|e| BatchWriterError::WriteError(e.to_string()))?;

        if task.requires_flush {
            write_stream.flush()
                .await
                .map_err(|e| BatchWriterError::WriteError(e.to_string()))?;
        }

        Ok(task.data.len())
    }

    /// Запуск обработчика очереди записи
    fn start_queue_handler(&self) {
        let mut batch_writer = self.clone();

        tokio::spawn(async move {
            batch_writer.process_write_queue().await;
        });
    }

    /// Обработка очереди записи - исправленная версия
    async fn process_write_queue(&mut self) {
        info!("🔄 BatchWriter queue processor started");

        let mut pending_batches: HashMap<std::net::SocketAddr, Vec<WriteTask>> = HashMap::new();

        // Создаем отдельный receiver для обработки
        let mut write_rx = self.write_queue_rx.lock().await;

        loop {
            tokio::select! {
                // Получение задачи из очереди
                Some(task) = write_rx.recv() => {
                    let addr = task.destination_addr;

                    // Добавляем задачу в соответствующий батч
                    let batch = pending_batches.entry(addr).or_insert_with(Vec::new);
                    batch.push(task);

                    // Проверяем, готов ли батч к записи
                    if batch.len() >= self.config.batch_size {
                        if let Some(batch_tasks) = pending_batches.remove(&addr) {
                            self.process_write_batch(addr, batch_tasks).await;
                        }
                    }
                }

                // Автоматический сброс по таймеру - исправленная версия
                _ = async {
                    let mut flush_timer = self.flush_timer.lock().await;
                    flush_timer.tick().await;
                } => {
                    // Сбрасываем все накопленные батчи
                    for (addr, batch_tasks) in pending_batches.drain() {
                        if !batch_tasks.is_empty() {
                            self.process_write_batch(addr, batch_tasks).await;
                        }
                    }
                }
            }
        }
    }

    /// Обработка батча записей
    async fn process_write_batch(&self, destination_addr: std::net::SocketAddr, tasks: Vec<WriteTask>) {
        let batch_id = self.batch_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let batch_start = Instant::now();
        let batch_size = tasks.len();

        if batch_size == 0 {
            return;
        }

        debug!("📤 Processing write batch #{} for {}: {} tasks",
           batch_id, destination_addr, batch_size);

        // Сортируем задачи по приоритету
        let mut sorted_tasks = tasks;
        sorted_tasks.sort_by_key(|t| t.priority);

        // Объединяем данные для пакетной записи
        let mut combined_data = BytesMut::new();
        let mut requires_flush = false;

        for task in &sorted_tasks {
            combined_data.extend_from_slice(&task.data);
            if task.requires_flush {
                requires_flush = true;
            }
        }

        // Выполняем пакетную запись
        match self.write_batch_data(destination_addr, combined_data.freeze(), requires_flush).await {
            Ok(bytes_written) => {
                // Освобождаем backpressure permits
                self.backpressure_semaphore.add_permits(batch_size);

                // Обновляем статистику
                self.update_statistics(batch_size, bytes_written, batch_start).await;

                // Отправляем событие о завершении
                self.send_write_completed(destination_addr, bytes_written, batch_start).await;

                debug!("✅ Write batch #{} completed: {} bytes in {:?}",
                   batch_id, bytes_written, batch_start.elapsed());
            }
            Err(e) => {
                error!("❌ Write batch #{} failed for {}: {}",
                   batch_id, destination_addr, e);

                self.send_write_error(destination_addr, e.to_string()).await;
            }
        }
    }

    /// Пакетная запись данных
    async fn write_batch_data(
        &self,
        destination_addr: std::net::SocketAddr,
        data: Bytes,
        requires_flush: bool,
    ) -> Result<usize, BatchWriterError> {
        let mut writers = self.active_writers.write().await;
        let writer_opt = writers.get_mut(&destination_addr);

        if let Some(writer) = writer_opt {
            let mut write_stream = &mut *writer.write_stream;

            // Пакетная запись через frame_writer
            frame_writer::write_frame(&mut write_stream, &data)
                .await
                .map_err(|e| BatchWriterError::WriteError(e.to_string()))?;

            if requires_flush {
                write_stream.flush()
                    .await
                    .map_err(|e| BatchWriterError::WriteError(e.to_string()))?;
            }

            Ok(data.len())
        } else {
            Err(BatchWriterError::ConnectionNotFound)
        }
    }

    /// Запуск таймера сброса
    fn start_flush_timer(&self) {
        let batch_writer = self.clone();

        tokio::spawn(async move {
            let mut flush_timer = interval(batch_writer.config.flush_interval);
            flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                flush_timer.tick().await;

                // Принудительный сброс всех буферов
                batch_writer.force_flush_all().await;
            }
        });
    }

    /// Принудительный сброс всех буферов
    async fn force_flush_all(&self) {
        let writers = self.active_writers.read().await;

        for writer in writers.values() {
            if writer.buffer_size > 0 {
                debug!("Force flushing buffer for {}: {} bytes",
                       writer.destination_addr, writer.buffer_size);

                // TODO: Реализовать принудительную запись буфера
            }
        }
    }

    /// Отправка события о завершении записи
    async fn send_write_completed(
        &self,
        destination_addr: std::net::SocketAddr,
        bytes_written: usize,
        start_time: Instant,
    ) {
        let batch_id = self.batch_counter.load(std::sync::atomic::Ordering::Relaxed);

        let event = BatchWriterEvent::WriteCompleted {
            destination_addr,
            batch_id,
            bytes_written,
            write_time: start_time.elapsed(),
        };

        self.event_tx.send(event).await.ok();
    }

    /// Отправка события об ошибке записи
    async fn send_write_error(&self, destination_addr: std::net::SocketAddr, error: String) {
        let event = BatchWriterEvent::WriteError {
            destination_addr,
            error,
        };

        self.event_tx.send(event).await.ok();

        let mut stats = self.stats.lock().await;
        stats.write_errors += 1;
    }

    /// Обновление статистики
    async fn update_statistics(&self, batch_size: usize, bytes_written: usize, batch_start: Instant) {
        let mut stats = self.stats.lock().await;

        stats.total_writes += batch_size as u64;
        stats.total_bytes_written += bytes_written as u64;
        stats.total_batches_written += 1;

        // Обновляем средний размер батча
        let total_batches = stats.total_batches_written as f64;
        stats.avg_batch_size =
            (stats.avg_batch_size * (total_batches - 1.0) + batch_size as f64) / total_batches;

        // Обновляем среднее время записи
        let write_time = batch_start.elapsed();
        stats.avg_write_time = Duration::from_nanos(
            ((stats.avg_write_time.as_nanos() as f64 * (total_batches - 1.0) +
                write_time.as_nanos() as f64) / total_batches) as u64
        );

        // Расчет writes per second
        if write_time.as_micros() > 0 {
            let wps = batch_size as f64 / (write_time.as_micros() as f64 / 1_000_000.0);
            stats.writes_per_second = 0.7 * stats.writes_per_second + 0.3 * wps;
            stats.bytes_per_second = stats.writes_per_second * (bytes_written as f64 / batch_size as f64);
        }

        // Отправляем обновление статистики
        let stats_event = BatchWriterEvent::StatisticsUpdate {
            stats: stats.clone(),
        };

        self.event_tx.send(stats_event).await.ok();
    }

    /// Получение статистики
    pub async fn get_stats(&self) -> WriterStats {
        self.stats.lock().await.clone()
    }

    /// Остановка всех писателей
    pub async fn shutdown(&self) {
        // Принудительно сбрасываем все буферы
        self.force_flush_all().await;

        let mut writers = self.active_writers.write().await;

        // Деактивируем все соединения
        for writer in writers.values_mut() {
            writer.is_active = false;
        }

        info!("BatchWriter shutdown completed");
    }
}

impl Clone for BatchWriter {
    fn clone(&self) -> Self {
        let (write_tx, write_rx) = mpsc::channel(self.config.max_pending_writes);
        let mut flush_timer = interval(self.config.flush_interval);
        flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        Self {
            config: self.config.clone(),
            event_tx: self.event_tx.clone(),
            active_writers: Arc::new(RwLock::new(HashMap::new())),
            write_queue: write_tx,
            write_queue_rx: Mutex::new(write_rx),
            stats: Mutex::new(WriterStats::default()),
            batch_counter: std::sync::atomic::AtomicU64::new(0),
            flush_timer: Mutex::new(flush_timer),
            backpressure_semaphore: Arc::new(Semaphore::new(self.config.max_pending_writes)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BatchWriterError {
    #[error("Connection already registered")]
    ConnectionAlreadyRegistered,
    #[error("Connection not found")]
    ConnectionNotFound,
    #[error("Write error: {0}")]
    WriteError(String),
    #[error("Queue error: {0}")]
    QueueError(String),
    #[error("Backpressure: too many pending writes")]
    Backpressure,
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}