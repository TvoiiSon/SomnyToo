use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::VecDeque;
use tokio::io::AsyncRead;
use tokio::sync::{mpsc, Mutex, RwLock};
use bytes::BytesMut;
use tracing::{info, debug, error, warn};

use crate::core::protocol::packets::frame_reader;
use crate::core::protocol::batch_system::config::BatchConfig;
use crate::core::protocol::batch_system::types::error::BatchError;
use crate::core::protocol::batch_system::types::priority::Priority;

/// Модель распределения размера пакетов (логнормальное распределение)
#[derive(Debug, Clone)]
pub struct PacketSizeModel {
    /// Среднее значение логарифма (μ)
    pub mu: f64,
    /// Стандартное отклонение логарифма (σ)
    pub sigma: f64,
    /// Средний размер пакета (E[X])
    pub mean: f64,
    /// Медианный размер пакета
    pub median: f64,
    /// 95-й перцентиль
    pub p95: f64,
    /// 99-й перцентиль
    pub p99: f64,
    /// История размеров
    pub history: VecDeque<usize>,
    /// Максимальный размер истории
    pub max_history: usize,
}

impl PacketSizeModel {
    pub fn new(max_history: usize) -> Self {
        Self {
            mu: 5.0,        // ~148 байт
            sigma: 1.0,     // типичное отклонение
            mean: 256.0,
            median: 128.0,
            p95: 1024.0,
            p99: 2048.0,
            history: VecDeque::with_capacity(max_history),
            max_history,
        }
    }

    /// Обновление модели на основе нового размера
    pub fn update(&mut self, size: usize) {
        self.history.push_back(size);
        if self.history.len() > self.max_history {
            self.history.pop_front();
        }

        if self.history.len() >= 100 {
            self.estimate_parameters();
        }
    }

    /// Оценка параметров логнормального распределения
    pub fn estimate_parameters(&mut self) {
        let sizes: Vec<f64> = self.history.iter()
            .map(|&s| (s as f64).ln())
            .collect();

        if sizes.is_empty() {
            return;
        }

        // Оценка μ (среднее логарифмов)
        self.mu = sizes.iter().sum::<f64>() / sizes.len() as f64;

        // Оценка σ (стандартное отклонение логарифмов)
        let variance = sizes.iter()
            .map(|&x| (x - self.mu).powi(2))
            .sum::<f64>() / (sizes.len() - 1) as f64;
        self.sigma = variance.sqrt();

        // Расчёт характеристик
        self.mean = (self.mu + self.sigma.powi(2) / 2.0).exp();
        self.median = self.mu.exp();
        self.p95 = (self.mu + 1.645 * self.sigma).exp();
        self.p99 = (self.mu + 2.326 * self.sigma).exp();
    }
}

#[derive(Debug, Clone)]
pub struct ReadIntensityModel {
    /// Текущая интенсивность (λ)
    pub lambda: f64,
    /// Пиковая интенсивность
    pub peak_lambda: f64,
    /// Средняя интенсивность
    pub avg_lambda: f64,
    /// Коэффициент вариации
    pub cv: f64,
    /// История интенсивности
    pub history: VecDeque<f64>,
    /// Временные метки
    pub timestamps: VecDeque<Instant>,
}

impl ReadIntensityModel {
    pub fn new() -> Self {
        Self {
            lambda: 0.0,
            peak_lambda: 0.0,
            avg_lambda: 0.0,
            cv: 0.0,
            history: VecDeque::with_capacity(1000),
            timestamps: VecDeque::with_capacity(1000),
        }
    }

    /// Обновление интенсивности
    pub fn update(&mut self, now: Instant) {
        // Очистка старых записей (> 1 минута)
        while let Some(&ts) = self.timestamps.front() {
            if now.duration_since(ts) > Duration::from_secs(60) {
                self.timestamps.pop_front();
                self.history.pop_front();
            } else {
                break;
            }
        }

        let window = 60.0; // секунд
        let count = self.timestamps.len();
        self.lambda = count as f64 / window;
        self.avg_lambda = self.history.iter().sum::<f64>() / self.history.len().max(1) as f64;
        self.peak_lambda = self.peak_lambda.max(self.lambda);

        if self.history.len() > 1 {
            let mean = self.avg_lambda;
            let variance = self.history.iter()
                .map(|&x| (x - mean).powi(2))
                .sum::<f64>() / (self.history.len() - 1) as f64;
            self.cv = variance.sqrt() / mean.max(0.001);
        }
    }

    /// Добавление нового чтения
    pub fn add_read(&mut self, timestamp: Instant) {
        self.timestamps.push_back(timestamp);
        self.history.push_back(self.lambda);
        if self.history.len() > 1000 {
            self.history.pop_front();
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReadTimeoutModel {
    /// Базовый таймаут
    pub base_timeout: Duration,
    /// Минимальный таймаут
    pub min_timeout: Duration,
    /// Максимальный таймаут
    pub max_timeout: Duration,
    /// Текущий таймаут
    pub current_timeout: Duration,
    /// Коэффициент адаптации
    pub adaptation_factor: f64,
    /// История времен чтения
    pub read_times: VecDeque<Duration>,
}

impl ReadTimeoutModel {
    pub fn new(base_timeout: Duration) -> Self {
        Self {
            base_timeout,
            min_timeout: Duration::from_millis(100),
            max_timeout: Duration::from_secs(30),
            current_timeout: base_timeout,
            adaptation_factor: 0.3,
            read_times: VecDeque::with_capacity(100),
        }
    }

    /// Обновление модели на основе времени чтения
    pub fn update(&mut self, read_time: Duration) {
        // ИСПРАВЛЕНО: Игнорируем timeout ошибки при адаптации
        if read_time >= self.current_timeout {
            debug!("⏸️ Not adapting timeout - operation timed out");
            return;
        }

        self.read_times.push_back(read_time);
        if self.read_times.len() > 100 {
            self.read_times.pop_front();
        }

        if self.read_times.len() >= 10 {
            let mut times: Vec<u64> = self.read_times.iter()
                .map(|d| d.as_millis() as u64)
                .collect();
            times.sort_unstable();
            let p95 = times[times.len() * 95 / 100];

            // ИСПРАВЛЕНО: Никогда не уменьшаем таймаут ниже 5 секунд
            let target = (p95 as f64 * 1.5).max(5000.0) as u64;  // минимум 5 сек
            let current = self.current_timeout.as_millis() as u64;

            // ИСПРАВЛЕНО: Только увеличиваем, не уменьшаем
            if target > current {
                let new_timeout = (current as f64 * (1.0 - self.adaptation_factor) +
                    target as f64 * self.adaptation_factor) as u64;

                self.current_timeout = Duration::from_millis(
                    new_timeout.clamp(5000, self.max_timeout.as_millis() as u64)
                );

                debug!("📈 Read timeout increased to {:?}", self.current_timeout);
            }
        }
    }

    /// Получение текущего таймаута
    pub fn get_timeout(&self) -> Duration {
        // ИСПРАВЛЕНО: Для новых соединений используем базовый таймаут
        if self.read_times.is_empty() {
            self.base_timeout
        } else {
            self.current_timeout
        }
    }

    /// Сброс к базовому значению
    pub fn reset(&mut self) {
        self.current_timeout = self.base_timeout;
        self.read_times.clear();
    }
}

#[derive(Debug, Clone)]
pub struct ReaderStats {
    pub total_bytes_read: u64,
    pub total_packets_read: u64,
    pub avg_packet_size: f64,
    pub p95_packet_size: f64,
    pub p99_packet_size: f64,
    pub read_rate: f64,          // пакетов/сек
    pub byte_rate: f64,          // байт/сек
    pub avg_read_time: Duration,
    pub p95_read_time: Duration,
    pub p99_read_time: Duration,
    pub timeout_count: u64,
    pub error_count: u64,
    pub connection_count: usize,
}

impl Default for ReaderStats {
    fn default() -> Self {
        Self {
            total_bytes_read: 0,
            total_packets_read: 0,
            avg_packet_size: 0.0,
            p95_packet_size: 0.0,
            p99_packet_size: 0.0,
            read_rate: 0.0,
            byte_rate: 0.0,
            avg_read_time: Duration::from_micros(0),
            p95_read_time: Duration::from_micros(0),
            p99_read_time: Duration::from_micros(0),
            timeout_count: 0,
            error_count: 0,
            connection_count: 0,
        }
    }
}

#[derive(Debug)]
pub enum ReaderEvent {
    DataReady {
        session_id: Vec<u8>,
        data: BytesMut,
        source_addr: std::net::SocketAddr,
        priority: Priority,
        received_at: Instant,
        size: usize,
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

pub struct BatchReader {
    config: BatchConfig,
    packet_size_model: Arc<RwLock<PacketSizeModel>>,
    intensity_model: Arc<RwLock<ReadIntensityModel>>,
    timeout_model: Arc<RwLock<ReadTimeoutModel>>,
    event_tx: mpsc::Sender<ReaderEvent>,
    is_running: Arc<std::sync::atomic::AtomicBool>,
    stats: Arc<RwLock<ReaderStats>>,
    connection_count: Arc<std::sync::atomic::AtomicUsize>,
}

impl BatchReader {
    pub fn new(config: BatchConfig, event_tx: mpsc::Sender<ReaderEvent>) -> Self {
        info!("📖 Initializing Mathematical BatchReader v2.0");
        info!("  Buffer size: {} KB", config.read_buffer_size / 1024);
        info!("  Base timeout: {:?}", config.read_timeout);
        info!("  Adaptive timeout: {}", config.adaptive_read_timeout);

        let timeout_model = ReadTimeoutModel::new(config.read_timeout);

        Self {
            config,
            packet_size_model: Arc::new(RwLock::new(PacketSizeModel::new(1000))),
            intensity_model: Arc::new(RwLock::new(ReadIntensityModel::new())),
            timeout_model: Arc::new(RwLock::new(timeout_model)),
            event_tx,
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            stats: Arc::new(RwLock::new(ReaderStats::default())),
            connection_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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

        self.connection_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let event_tx = self.event_tx.clone();
        let is_running = self.is_running.clone();
        let session_id_clone = session_id.clone();

        // Исправление: перемещаем Arc'ы в задачу, а не self
        let packet_size_model = self.packet_size_model.clone();
        let intensity_model = self.intensity_model.clone();
        let timeout_model = self.timeout_model.clone();
        let stats = self.stats.clone();
        let connection_count = self.connection_count.clone();

        tokio::spawn(async move {
            let read_stream = Arc::new(Mutex::new(read_stream));
            let session_id_inner = session_id_clone.clone();
            let mut consecutive_read_errors = 0;
            const MAX_CONSECUTIVE_ERRORS: u32 = 10;

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                let read_start = Instant::now();

                // Получение текущего таймаута
                let timeout = {
                    let model = timeout_model.read().await;
                    model.get_timeout()
                };

                let read_result = {
                    let mut stream_guard = read_stream.lock().await;
                    // Исправление: передаём ссылку на читаемый поток
                    Self::read_from_stream_dyn(&mut *stream_guard, timeout).await
                };

                match read_result {
                    Ok(Some((data, bytes_read))) => {
                        consecutive_read_errors = 0;

                        // Обновление математических моделей
                        {
                            let mut psm = packet_size_model.write().await;
                            psm.update(bytes_read);
                        }

                        {
                            let mut im = intensity_model.write().await;
                            im.add_read(Instant::now());
                            im.update(Instant::now());
                        }

                        {
                            let mut tm = timeout_model.write().await;
                            tm.update(read_start.elapsed());
                        }

                        // Обновление статистики
                        {
                            let mut s = stats.write().await;
                            s.total_bytes_read += bytes_read as u64;
                            s.total_packets_read += 1;

                            // EMA для среднего размера
                            let alpha = 0.1;
                            s.avg_packet_size = s.avg_packet_size * (1.0 - alpha) + bytes_read as f64 * alpha;
                        }

                        let priority = Priority::from_byte(&data);

                        let event = ReaderEvent::DataReady {
                            session_id: session_id_inner.clone(),
                            data,
                            source_addr,
                            priority,
                            received_at: Instant::now(),
                            size: bytes_read,
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
                            BatchError::Timeout => {
                                let mut s = stats.write().await;
                                s.timeout_count += 1;
                            }
                            _ => {
                                let mut s = stats.write().await;
                                s.error_count += 1;
                            }
                        }

                        warn!("⚠️ Read error from {} (attempt {}): {}",
                            source_addr, consecutive_read_errors, e);

                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }

                // Небольшая задержка для предотвращения spin-loop
                tokio::time::sleep(Duration::from_millis(1)).await;
            }

            // Обновление счётчика соединений
            connection_count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

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
        timeout: Duration,
    ) -> Result<Option<(BytesMut, usize)>, BatchError> {
        match tokio::time::timeout(
            timeout,
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

    pub async fn get_stats(&self) -> ReaderStats {
        let mut stats = self.stats.write().await;

        // Расчёт read_rate
        let intensity = self.intensity_model.read().await;
        stats.read_rate = intensity.lambda;
        stats.byte_rate = stats.total_bytes_read as f64 / 60.0; // байт/сек за последнюю минуту

        // Расчёт перцентилей размера пакетов
        let psm = self.packet_size_model.read().await;
        stats.p95_packet_size = psm.p95;
        stats.p99_packet_size = psm.p99;

        // Расчёт перцентилей времени чтения
        let tm = self.timeout_model.read().await;
        let mut times: Vec<u64> = tm.read_times.iter()
            .map(|d| d.as_micros() as u64)
            .collect();

        if !times.is_empty() {
            times.sort_unstable();
            let len = times.len();
            stats.avg_read_time = Duration::from_micros(
                times.iter().sum::<u64>() / len as u64
            );
            stats.p95_read_time = Duration::from_micros(times[len * 95 / 100]);
            stats.p99_read_time = Duration::from_micros(times[len * 99 / 100]);
        }

        stats.connection_count = self.connection_count.load(std::sync::atomic::Ordering::Relaxed);

        stats.clone()
    }

    pub async fn shutdown(&self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
        info!("📖 BatchReader shutdown initiated");
    }
}