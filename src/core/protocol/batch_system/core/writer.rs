use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::{VecDeque, BinaryHeap};
use std::cmp::Ordering;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{RwLock, Semaphore, Mutex, mpsc};
use bytes::Bytes;
use tracing::{info, debug, error, warn};

use crate::core::protocol::packets::frame_writer;
use crate::core::protocol::batch_system::config::BatchConfig;
use crate::core::protocol::batch_system::types::error::BatchError;
use crate::core::protocol::batch_system::types::priority::Priority;

/// Модель батчинга записи
#[derive(Debug, Clone)]
pub struct WriteBatchModel {
    /// Текущий размер батча
    pub current_batch_size: usize,
    /// Оптимальный размер батча
    pub optimal_batch_size: usize,
    /// Минимальный размер батча
    pub min_batch_size: usize,
    /// Максимальный размер батча
    pub max_batch_size: usize,
    /// Время накопления батча
    pub accumulation_time: Duration,
    /// Время записи батча
    pub write_time: Duration,
    /// Целевая задержка
    pub target_latency: Duration,
    /// История производительности
    pub history: VecDeque<BatchWriteRecord>,
}

#[derive(Debug, Clone)]
pub struct BatchWriteRecord {
    pub batch_size: usize,
    pub write_time: Duration,
    pub throughput: f64,
    pub timestamp: Instant,
}

impl WriteBatchModel {
    pub fn new(min_size: usize, max_size: usize, target_latency: Duration) -> Self {
        Self {
            current_batch_size: min_size,
            optimal_batch_size: min_size,
            min_batch_size: min_size,
            max_batch_size: max_size,
            accumulation_time: Duration::from_millis(10),
            write_time: Duration::from_micros(0),
            target_latency,
            history: VecDeque::with_capacity(100),
        }
    }

    /// Обновление модели на основе измерения
    pub fn update(&mut self, batch_size: usize, write_time: Duration) {
        self.history.push_back(BatchWriteRecord {
            batch_size,
            write_time,
            throughput: batch_size as f64 / write_time.as_secs_f64().max(0.0001),
            timestamp: Instant::now(),
        });

        if self.history.len() > 100 {
            self.history.pop_front();
        }

        self.current_batch_size = batch_size;
        self.write_time = write_time;

        self.estimate_optimal_batch_size();
    }

    /// Оценка оптимального размера батча
    fn estimate_optimal_batch_size(&mut self) {
        if self.history.len() < 10 {
            return;
        }

        let mut best_throughput = 0.0;
        let mut best_size = self.current_batch_size;

        for record in self.history.iter() {
            if record.throughput > best_throughput &&
                record.write_time <= self.target_latency {
                best_throughput = record.throughput;
                best_size = record.batch_size;
            }
        }

        self.optimal_batch_size = best_size;
    }
}

#[derive(Debug, Clone)]
pub struct BackpressureModel {
    /// Максимальный размер очереди (K)
    pub max_queue_size: usize,
    /// Текущий размер очереди
    pub current_queue: usize,
    /// Интенсивность поступления (λ)
    pub arrival_rate: f64,
    /// Интенсивность обслуживания (μ)
    pub service_rate: f64,
    /// Коэффициент загрузки (ρ)
    pub utilization: f64,
    /// Вероятность потери пакета
    pub loss_probability: f64,
    /// Среднее время ожидания
    pub avg_wait_time: Duration,
}

impl BackpressureModel {
    pub fn new(max_queue_size: usize) -> Self {
        Self {
            max_queue_size,
            current_queue: 0,
            arrival_rate: 0.0,
            service_rate: 1000.0,
            utilization: 0.0,
            loss_probability: 0.0,
            avg_wait_time: Duration::from_micros(0),
        }
    }

    /// Обновление модели
    pub fn update(&mut self, queue_size: usize, arrival_rate: f64, service_rate: f64) {
        self.current_queue = queue_size;
        self.arrival_rate = arrival_rate;
        self.service_rate = service_rate;
        self.utilization = arrival_rate / service_rate.max(0.001);

        // M/M/1/K формула для вероятности потери
        if (self.utilization - 1.0).abs() < 0.001 {
            self.loss_probability = 1.0 / (self.max_queue_size + 1) as f64;
        } else {
            let rho = self.utilization;
            self.loss_probability = (rho.powi(self.max_queue_size as i32 + 1) * (1.0 - rho)) /
                (1.0 - rho.powi(self.max_queue_size as i32 + 2));
        }

        // Среднее время ожидания (Little's Law)
        if arrival_rate > 0.0 {
            let avg_queue = if self.utilization < 1.0 {
                self.utilization / (1.0 - self.utilization)
            } else {
                self.max_queue_size as f64
            };
            self.avg_wait_time = Duration::from_secs_f64(avg_queue / arrival_rate);
        }
    }

    /// Достигнут ли порог backpressure
    pub fn is_backpressure(&self) -> bool {
        self.current_queue >= self.max_queue_size / 2
    }

    /// Критический уровень backpressure
    pub fn is_critical(&self) -> bool {
        self.current_queue >= self.max_queue_size * 9 / 10
    }
}

#[derive(Debug, Clone)]
pub struct PrioritizedWriteTask {
    pub task: WriteTask,
    pub enqueue_time: Instant,
    pub priority_score: i32,
}

impl Eq for PrioritizedWriteTask {}

impl PartialEq for PrioritizedWriteTask {
    fn eq(&self, other: &Self) -> bool {
        self.priority_score == other.priority_score
    }
}

impl PartialOrd for PrioritizedWriteTask {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrioritizedWriteTask {
    fn cmp(&self, other: &Self) -> Ordering {
        // Высокий приоритет = меньший score (для BinaryHeap max-heap)
        other.priority_score.cmp(&self.priority_score)
    }
}

#[derive(Debug, Clone)]
pub struct WriteTask {
    pub destination_addr: std::net::SocketAddr,
    pub session_id: Vec<u8>,
    pub data: Bytes,
    pub priority: Priority,
    pub requires_flush: bool,
    pub created_at: Instant,
    pub size_bytes: usize,
}

impl WriteTask {
    pub fn priority_score(&self) -> i32 {
        match self.priority {
            Priority::Critical => 1000,
            Priority::High => 750,
            Priority::Normal => 500,
            Priority::Low => 250,
            Priority::Background => 100,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WriterStats {
    pub total_bytes_written: u64,
    pub total_packets_written: u64,
    pub total_batches_written: u64,
    pub avg_batch_size: f64,
    pub optimal_batch_size: usize,
    pub avg_write_time: Duration,
    pub p95_write_time: Duration,
    pub p99_write_time: Duration,
    pub write_rate: f64,        // пакетов/сек
    pub byte_rate: f64,         // байт/сек
    pub queue_size: usize,
    pub max_queue_size: usize,
    pub backpressure_events: u64,
    pub timeout_events: u64,
    pub error_events: u64,
    pub connection_count: usize,
}

impl Default for WriterStats {
    fn default() -> Self {
        Self {
            total_bytes_written: 0,
            total_packets_written: 0,
            total_batches_written: 0,
            avg_batch_size: 0.0,
            optimal_batch_size: 32,
            avg_write_time: Duration::from_micros(0),
            p95_write_time: Duration::from_micros(0),
            p99_write_time: Duration::from_micros(0),
            write_rate: 0.0,
            byte_rate: 0.0,
            queue_size: 0,
            max_queue_size: 0,
            backpressure_events: 0,
            timeout_events: 0,
            error_events: 0,
            connection_count: 0,
        }
    }
}

pub struct BatchWriter {
    config: BatchConfig,
    batch_model: Arc<RwLock<WriteBatchModel>>,
    backpressure_model: Arc<RwLock<BackpressureModel>>,
    connections: Arc<RwLock<Vec<ConnectionWriter>>>,
    task_queue: Arc<Mutex<BinaryHeap<PrioritizedWriteTask>>>,
    task_tx: mpsc::Sender<WriteTask>,
    task_rx: Arc<Mutex<mpsc::Receiver<WriteTask>>>,
    backpressure_semaphore: Arc<Semaphore>,
    is_running: Arc<std::sync::atomic::AtomicBool>,
    stats: Arc<RwLock<WriterStats>>,
    connection_count: Arc<std::sync::atomic::AtomicUsize>,
    write_times: Arc<Mutex<VecDeque<Duration>>>,
}

struct ConnectionWriter {
    destination_addr: std::net::SocketAddr,
    session_id: Vec<u8>,
    write_stream: Box<dyn AsyncWrite + Unpin + Send + Sync>,
    last_write_time: Instant,
    is_active: bool,
    bytes_written: u64,
    packets_written: u64,
}

impl BatchWriter {
    pub fn new(config: BatchConfig) -> Self {
        info!("📝 Initializing Mathematical BatchWriter v2.0");
        info!("  Buffer size: {} KB", config.write_buffer_size / 1024);
        info!("  Batch size: {}", config.batch_size);
        info!("  Flush interval: {:?}", config.flush_interval);

        let (task_tx, task_rx) = mpsc::channel(config.max_pending_writes);

        let batch_model = WriteBatchModel::new(
            config.min_batch_size,
            config.max_batch_size,
            config.write_timeout,
        );

        let backpressure_model = BackpressureModel::new(config.max_pending_writes);

        Self {
            config: config.clone(),
            batch_model: Arc::new(RwLock::new(batch_model)),
            backpressure_model: Arc::new(RwLock::new(backpressure_model)),
            connections: Arc::new(RwLock::new(Vec::new())),
            task_queue: Arc::new(Mutex::new(BinaryHeap::with_capacity(config.max_pending_writes))),
            task_tx,
            task_rx: Arc::new(Mutex::new(task_rx)),
            backpressure_semaphore: Arc::new(Semaphore::new(config.max_pending_writes)),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            stats: Arc::new(RwLock::new(WriterStats::default())),
            connection_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            write_times: Arc::new(Mutex::new(VecDeque::with_capacity(1000))),
        }
    }

    pub async fn register_connection(
        &self,
        destination_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        write_stream: Box<dyn AsyncWrite + Unpin + Send + Sync>,
    ) -> Result<(), BatchError> {
        self.connection_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let writer = ConnectionWriter {
            destination_addr,
            session_id: session_id.clone(),
            write_stream,
            last_write_time: Instant::now(),
            is_active: true,
            bytes_written: 0,
            packets_written: 0,
        };

        {
            let mut connections = self.connections.write().await;
            connections.push(writer);
        }

        self.start_writer_for_connection(destination_addr).await?;

        let mut stats = self.stats.write().await;
        stats.connection_count = self.connection_count.load(std::sync::atomic::Ordering::Relaxed);

        debug!("🔗 Writer connection registered: {} -> {}",
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

        // Проверка backpressure
        let permit = self.backpressure_semaphore.clone()
            .try_acquire_owned()
            .map_err(|_| {
                warn!("⚠️ Backpressure for {}", destination_addr);
                BatchError::Backpressure
            })?;

        let task = WriteTask {
            destination_addr,
            session_id,
            data: data.clone(),
            priority,
            requires_flush,
            created_at: Instant::now(),
            size_bytes: data.len(),
        };

        // Критические задачи отправляем немедленно
        if priority.is_critical() || requires_flush {
            let result = self.write_immediate(task).await;
            drop(permit);
            return result;
        }

        // ИСПРАВЛЕНО: mpsc::Sender::send() возвращает Result, но мы .await его
        if let Err(e) = self.task_tx.send(task).await {
            error!("❌ Failed to queue write task: {}", e);
            drop(permit);
            return Err(BatchError::Backpressure);
        }

        drop(permit);
        Ok(())
    }

    async fn write_immediate(&self, task: WriteTask) -> Result<(), BatchError> {
        let mut connections = self.connections.write().await;

        if let Some(writer) = connections.iter_mut()
            .find(|w| w.session_id == task.session_id && w.is_active) {

            debug!("⚡ Immediate write to session {}", hex::encode(&task.session_id));

            let write_start = Instant::now();

            match tokio::time::timeout(
                self.config.write_timeout,
                frame_writer::write_frame(&mut writer.write_stream, &task.data),
            ).await {
                Ok(Ok(_)) => {
                    if task.requires_flush {
                        writer.write_stream.flush().await
                            .map_err(BatchError::Io)?;
                    }

                    let write_time = write_start.elapsed();

                    // Обновление статистики
                    writer.last_write_time = Instant::now();
                    writer.bytes_written += task.size_bytes as u64;
                    writer.packets_written += 1;

                    {
                        let mut stats = self.stats.write().await;
                        stats.total_bytes_written += task.size_bytes as u64;
                        stats.total_packets_written += 1;

                        let alpha = 0.1;
                        stats.avg_write_time = Duration::from_nanos(
                            (stats.avg_write_time.as_nanos() as f64 * (1.0 - alpha) +
                                write_time.as_nanos() as f64 * alpha) as u64
                        );
                    }

                    {
                        let mut times = self.write_times.lock().await;
                        times.push_back(write_time);
                        if times.len() > 1000 {
                            times.pop_front();
                        }
                    }

                    Ok(())
                }
                Ok(Err(e)) => {
                    writer.is_active = false;
                    error!("❌ Immediate write failed: {}", e);

                    let mut stats = self.stats.write().await;
                    stats.error_events += 1;

                    Err(BatchError::ProcessingError(e.to_string()))
                }
                Err(_) => {
                    writer.is_active = false;
                    error!("⏰ Immediate write timeout");

                    let mut stats = self.stats.write().await;
                    stats.timeout_events += 1;

                    Err(BatchError::Timeout)
                }
            }
        } else {
            error!("❌ Connection not found for session {}", hex::encode(&task.session_id));
            Err(BatchError::ConnectionError("Connection not found".to_string()))
        }
    }

    pub async fn start_writer_for_connection(&self, destination_addr: std::net::SocketAddr) -> Result<(), BatchError> {
        // ИСПРАВЛЕНО: клонируем все Arc ДО tokio::spawn!
        let connections = self.connections.clone();
        let config = self.config.clone();
        let is_running = self.is_running.clone();
        let backpressure = self.backpressure_semaphore.clone();
        let batch_model = self.batch_model.clone();
        let stats = self.stats.clone();

        // ИСПРАВЛЕНО: для mpsc Receiver нужно clone через мутекс
        let task_rx = self.task_rx.clone();

        tokio::spawn(async move {
            // ИСПРАВЛЕНО: получаем receiver внутри задачи
            let mut task_rx = task_rx.lock().await;
            let mut pending_tasks: Vec<WriteTask> = Vec::with_capacity(config.batch_size);
            let mut last_flush = Instant::now();

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                let optimal_size = {
                    let model = batch_model.read().await;
                    model.optimal_batch_size
                };

                tokio::select! {
                Some(task) = task_rx.recv() => {
                    if task.destination_addr == destination_addr {
                        pending_tasks.push(task);

                        let send_batch =
                            pending_tasks.len() >= optimal_size ||
                            last_flush.elapsed() >= config.flush_interval ||
                            pending_tasks.iter().any(|t| t.requires_flush);

                        if send_batch {
                            let batch_size = pending_tasks.len();
                            let write_start = Instant::now();

                            match Self::process_batch(
                                &connections,
                                destination_addr,
                                &mut pending_tasks,
                                &config
                            ).await {
                                Ok(_) => {
                                    let write_time = write_start.elapsed();

                                    let mut model = batch_model.write().await;
                                    model.update(batch_size, write_time);

                                    let mut s = stats.write().await;
                                    s.total_batches_written += 1;
                                    s.avg_batch_size = s.avg_batch_size * 0.9 + batch_size as f64 * 0.1;
                                    s.optimal_batch_size = model.optimal_batch_size;
                                }
                                Err(e) => {
                                    error!("Batch write error: {}", e);
                                }
                            }

                            backpressure.add_permits(batch_size);
                            pending_tasks.clear();
                            last_flush = Instant::now();
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    if !pending_tasks.is_empty() && last_flush.elapsed() >= config.flush_interval {
                        let batch_size = pending_tasks.len();
                        let write_start = Instant::now();

                        match Self::process_batch(
                            &connections,
                            destination_addr,
                            &mut pending_tasks,
                            &config
                        ).await {
                            Ok(_) => {
                                let write_time = write_start.elapsed();

                                let mut model = batch_model.write().await;
                                model.update(batch_size, write_time);

                                let mut s = stats.write().await;
                                s.total_batches_written += 1;
                                s.avg_batch_size = s.avg_batch_size * 0.9 + batch_size as f64 * 0.1;
                                s.optimal_batch_size = model.optimal_batch_size;
                            }
                            Err(e) => {
                                error!("Batch write error: {}", e);
                            }
                        }

                        backpressure.add_permits(batch_size);
                        pending_tasks.clear();
                        last_flush = Instant::now();
                    }
                }
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
            // Сортировка по приоритету
            tasks.sort_by(|a, b| b.priority.cmp(&a.priority));

            // Объединение данных
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

            // Адаптивный таймаут на основе размера батча
            let timeout = config.write_timeout * (tasks.len() as u32).max(1);

            match tokio::time::timeout(
                timeout,
                frame_writer::write_frame(&mut writer.write_stream, &data_bytes),
            ).await {
                Ok(Ok(_)) => {
                    writer.last_write_time = Instant::now();
                    writer.bytes_written += total_bytes as u64;
                    writer.packets_written += tasks.len() as u64;

                    if requires_flush {
                        debug!("🌀 Flushing batch for session {}", hex::encode(&writer.session_id));
                        writer.write_stream.flush().await
                            .map_err(BatchError::Io)?;
                    }

                    debug!("✅ Batch write to {}: {} tasks, {} bytes, {:?}",
                        destination_addr, tasks.len(), total_bytes, timeout);

                    Ok(())
                }
                Ok(Err(e)) => {
                    writer.is_active = false;
                    error!("❌ Batch write failed: {}", e);
                    Err(BatchError::ProcessingError(e.to_string()))
                }
                Err(_) => {
                    writer.is_active = false;
                    error!("⏰ Batch write timeout after {:?}", timeout);
                    Err(BatchError::Timeout)
                }
            }
        } else {
            error!("❌ Connection not found for batch write to {}", destination_addr);
            Err(BatchError::ConnectionError("Connection not found".to_string()))
        }
    }

    pub async fn get_stats(&self) -> WriterStats {
        let mut stats = self.stats.write().await;

        // Расчёт write_rate
        let uptime = self.config.write_timeout.as_secs_f64().max(1.0);
        stats.write_rate = stats.total_packets_written as f64 / uptime;
        stats.byte_rate = stats.total_bytes_written as f64 / uptime;

        // Расчёт перцентилей времени записи
        let times = self.write_times.lock().await;
        let mut times_us: Vec<u64> = times.iter()
            .map(|d| d.as_micros() as u64)
            .collect();

        if !times_us.is_empty() {
            times_us.sort_unstable();
            let len = times_us.len();
            stats.avg_write_time = Duration::from_micros(
                times_us.iter().sum::<u64>() / len as u64
            );
            stats.p95_write_time = Duration::from_micros(times_us[len * 95 / 100]);
            stats.p99_write_time = Duration::from_micros(times_us[len * 99 / 100]);
        }

        stats.connection_count = self.connection_count.load(std::sync::atomic::Ordering::Relaxed);

        // Текущий размер очереди
        let queue = self.task_queue.lock().await;
        stats.queue_size = queue.len();

        stats.clone()
    }

    pub async fn shutdown(&self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);

        let mut connections = self.connections.write().await;
        for connection in connections.iter_mut() {
            connection.is_active = false;
        }

        let mut queue = self.task_queue.lock().await;
        queue.clear();

        info!("📝 BatchWriter shutdown completed with {} connections", connections.len());
    }
}