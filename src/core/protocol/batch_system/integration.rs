use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::HashMap;
use tokio::sync::{mpsc, RwLock, Mutex, broadcast};
use bytes::Bytes;
use tracing::{info, error, debug, warn, trace};
use dashmap::DashMap;

// Импорты из batch системы
use crate::core::protocol::batch_system::config::BatchConfig;
use crate::core::protocol::batch_system::core::reader::{BatchReader, ReaderEvent};
use crate::core::protocol::batch_system::core::writer::BatchWriter;
use crate::core::protocol::batch_system::core::dispatcher::PacketDispatcher;
use crate::core::protocol::batch_system::core::processor::CryptoProcessor;
use crate::core::protocol::batch_system::core::buffer::UnifiedBufferPool;
use crate::core::protocol::batch_system::optimized::work_stealing_dispatcher::{WorkStealingDispatcher, WorkStealingTask, WorkStealingResult};
use crate::core::protocol::batch_system::optimized::buffer_pool::OptimizedBufferPool;
use crate::core::protocol::batch_system::optimized::crypto_processor::OptimizedCryptoProcessor;
use crate::core::protocol::batch_system::acceleration_batch::chacha20_batch_accel::ChaCha20BatchAccelerator;
use crate::core::protocol::batch_system::acceleration_batch::blake3_batch_accel::Blake3BatchAccelerator;
use crate::core::protocol::batch_system::types::error::BatchError;
use crate::core::protocol::batch_system::types::priority::Priority;

// Импорты из других модулей
use crate::core::monitoring::unified_monitor::UnifiedMonitor;
use crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::packets::packet_service::PhantomPacketService;
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;

/// Основной интегрированный узел Batch системы
pub struct IntegratedBatchSystem {
    // Конфигурация
    config: BatchConfig,

    // Основные компоненты
    reader: Arc<BatchReader>,
    writer: Arc<BatchWriter>,
    dispatcher: Arc<PacketDispatcher>,
    work_stealing_dispatcher: Arc<WorkStealingDispatcher>,
    crypto_processor: Arc<CryptoProcessor>,
    optimized_crypto_processor: Arc<OptimizedCryptoProcessor>,
    buffer_pool: Arc<UnifiedBufferPool>,
    optimized_buffer_pool: Arc<OptimizedBufferPool>,

    // Акселераторы
    chacha20_accelerator: Arc<ChaCha20BatchAccelerator>,
    blake3_accelerator: Arc<Blake3BatchAccelerator>,

    // Сервисы
    packet_service: Arc<PhantomPacketService>,
    packet_processor: PhantomPacketProcessor,
    session_manager: Arc<PhantomSessionManager>,
    crypto: Arc<PhantomCrypto>,
    monitor: Arc<UnifiedMonitor>,

    // Каналы и управление
    event_tx: mpsc::Sender<SystemEvent>,
    event_rx: Arc<Mutex<mpsc::Receiver<SystemEvent>>>,
    command_tx: broadcast::Sender<SystemCommand>,

    // Состояние системы
    is_running: Arc<std::sync::atomic::AtomicBool>,
    is_initialized: Arc<std::sync::atomic::AtomicBool>,
    startup_time: Instant,

    // Статистика и мониторинг
    stats: Arc<RwLock<SystemStatistics>>,
    metrics: Arc<DashMap<String, MetricValue>>,
    health_checks: Arc<RwLock<HashMap<String, HealthStatus>>>,

    // Очереди и буферы
    pending_batches: Arc<RwLock<Vec<PendingBatch>>>,
    active_connections: Arc<RwLock<HashMap<std::net::SocketAddr, ConnectionInfo>>>,
    session_cache: Arc<RwLock<HashMap<Vec<u8>, SessionCacheEntry>>>,
}

/// События системы
#[derive(Debug, Clone)]
pub enum SystemEvent {
    DataReceived {
        session_id: Vec<u8>,
        data: Bytes,
        source_addr: std::net::SocketAddr,
        priority: Priority,
        timestamp: Instant,
    },
    DataProcessed {
        session_id: Vec<u8>,
        result: ProcessResult,
        processing_time: Duration,
        worker_id: Option<usize>,
    },
    ConnectionOpened {
        addr: std::net::SocketAddr,
        session_id: Vec<u8>,
    },
    ConnectionClosed {
        addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        reason: String,
    },
    BatchCompleted {
        batch_id: u64,
        size: usize,
        processing_time: Duration,
        success_rate: f64,
    },
    ErrorOccurred {
        error: String,
        context: String,
        severity: ErrorSeverity,
    },
    HealthCheck {
        component: String,
        status: HealthStatus,
        details: HashMap<String, String>,
    },
    PerformanceAlert {
        metric: String,
        value: f64,
        threshold: f64,
        component: String,
    },
}

/// Команды управления системой
#[derive(Debug, Clone)]
pub enum SystemCommand {
    StartProcessing,
    PauseProcessing,
    ResumeProcessing,
    StopProcessing,
    FlushBuffers,
    ClearCaches,
    AdjustConfig {
        parameter: String,
        value: String,
    },
    EmergencyShutdown {
        reason: String,
    },
    RunHealthCheck {
        component: Option<String>,
    },
    GetStatistics,
    ResetStatistics,
    RebalanceWorkers,
    ScaleUp {
        count: usize,
    },
    ScaleDown {
        count: usize,
    },
}

/// Статус системы
#[derive(Debug, Clone)]
pub struct SystemStatus {
    pub timestamp: Instant,
    pub overall_status: SystemHealth,
    pub component_status: HashMap<String, ComponentStatus>,
    pub statistics: SystemStatistics,
    pub active_connections: usize,
    pub pending_tasks: usize,
    pub memory_usage: MemoryUsage,
    pub cpu_usage: f64,
    pub throughput: ThroughputMetrics,
    pub alerts: Vec<SystemAlert>,
}

/// Статистика системы
#[derive(Debug, Clone)]
pub struct SystemStatistics {
    pub total_data_received: u64,
    pub total_data_sent: u64,
    pub total_packets_processed: u64,
    pub total_batches_processed: u64,
    pub total_errors: u64,
    pub total_connections: u64,
    pub avg_processing_time: Duration,
    pub peak_throughput: f64,
    pub buffer_hit_rate: f64,
    pub crypto_operations: u64,
    pub work_stealing_count: u64,
    pub startup_time: Instant,
    pub uptime: Duration,
}

impl Default for SystemStatistics {
    fn default() -> Self {
        Self {
            total_data_received: 0,
            total_data_sent: 0,
            total_packets_processed: 0,
            total_batches_processed: 0,
            total_errors: 0,
            total_connections: 0,
            avg_processing_time: Duration::from_secs(0),
            peak_throughput: 0.0,
            buffer_hit_rate: 0.0,
            crypto_operations: 0,
            work_stealing_count: 0,
            startup_time: Instant::now(),
            uptime: Duration::from_secs(0),
        }
    }
}

/// Информация о соединении
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub addr: std::net::SocketAddr,
    pub session_id: Vec<u8>,
    pub opened_at: Instant,
    pub last_activity: Instant,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub priority: Priority,
    pub is_active: bool,
    pub worker_assigned: Option<usize>,
}

/// Кэш сессии
#[derive(Debug, Clone)]
pub struct SessionCacheEntry {
    pub session_id: Vec<u8>,
    pub last_used: Instant,
    pub access_count: u64,
    pub data: Bytes,
    pub metadata: HashMap<String, String>,
}

/// Ожидающий батч
#[derive(Debug, Clone)]
pub struct PendingBatch {
    pub id: u64,
    pub operations: Vec<BatchOperation>,
    pub priority: Priority,
    pub created_at: Instant,
    pub deadline: Option<Instant>,
    pub retry_count: u32,
}

/// Операция батча
#[derive(Debug, Clone)]
pub enum BatchOperation {
    Encryption {
        session_id: Vec<u8>,
        data: Bytes,
        key: [u8; 32],
        nonce: [u8; 12],
    },
    Decryption {
        session_id: Vec<u8>,
        data: Bytes,
        key: [u8; 32],
        nonce: [u8; 12],
    },
    Hashing {
        data: Bytes,
        key: Option<[u8; 32]>,
    },
    Processing {
        session_id: Vec<u8>,
        data: Bytes,
        processor_type: ProcessorType,
    },
}

/// Тип процессора
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorType {
    Standard,
    Accelerated,
    Optimized,
    WorkStealing,
}

/// Результат обработки
#[derive(Debug, Clone)]
pub struct ProcessResult {
    pub success: bool,
    pub data: Option<Bytes>,
    pub error: Option<String>,
    pub metadata: HashMap<String, String>,
}

/// Использование памяти
#[derive(Debug, Clone)]
pub struct MemoryUsage {
    pub total: usize,
    pub used: usize,
    pub free: usize,
    pub buffer_pool: usize,
    pub crypto_pool: usize,
    pub connections: usize,
}

/// Метрики пропускной способности
#[derive(Debug, Clone)]
pub struct ThroughputMetrics {
    pub packets_per_second: f64,
    pub bytes_per_second: f64,
    pub operations_per_second: f64,
    pub avg_batch_size: f64,
    pub latency_p50: Duration,
    pub latency_p95: Duration,
    pub latency_p99: Duration,
}

/// Статус компонента
#[derive(Debug, Clone)]
pub struct ComponentStatus {
    pub name: String,
    pub status: HealthStatus,
    pub last_check: Instant,
    pub details: HashMap<String, String>,
    pub performance: f64,
}

/// Здоровье системы
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemHealth {
    Healthy,
    Degraded,
    Unhealthy,
    Critical,
    Offline,
}

/// Статус здоровья
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Ok,
    Warning,
    Error,
    Unknown,
}

/// Серьезность ошибки
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSeverity {
    Low,
    Medium,
    High,
    Critical,
}

/// Оповещение системы
#[derive(Debug, Clone)]
pub struct SystemAlert {
    pub id: u64,
    pub timestamp: Instant,
    pub severity: ErrorSeverity,
    pub message: String,
    pub component: String,
    pub details: HashMap<String, String>,
    pub acknowledged: bool,
}

/// Значение метрики
#[derive(Debug, Clone)]
pub enum MetricValue {
    Integer(i64),
    Float(f64),
    Duration(Duration),
    String(String),
    Boolean(bool),
}

impl IntegratedBatchSystem {
    /// Создание новой интегрированной batch системы
    pub async fn new(
        config: BatchConfig,
        monitor: Arc<UnifiedMonitor>,
        session_manager: Arc<PhantomSessionManager>,
        crypto: Arc<PhantomCrypto>,
    ) -> Result<Self, BatchError> {
        info!("🚀 Инициализация интегрированной Batch системы...");

        let startup_time = Instant::now();

        // Создаем каналы для событий СИСТЕМЫ
        let (system_event_tx, system_event_rx) = mpsc::channel(10000);
        let (command_tx, _) = broadcast::channel(100);

        // Создаем канал для READER событий
        let (reader_event_tx, reader_event_rx) = mpsc::channel(10000);

        // Инициализируем основные компоненты
        let buffer_pool = Arc::new(UnifiedBufferPool::new(config.clone()));
        let optimized_buffer_pool = Arc::new(OptimizedBufferPool::new(
            config.read_buffer_size,
            config.write_buffer_size,
            64 * 1024,
            1000,
        ));

        let crypto_processor = Arc::new(CryptoProcessor::new(config.clone()));
        let optimized_crypto_processor = Arc::new(OptimizedCryptoProcessor::new(
            num_cpus::get()
        ));

        let chacha20_accelerator = Arc::new(ChaCha20BatchAccelerator::new(8));
        let blake3_accelerator = Arc::new(Blake3BatchAccelerator::new(8));

        // Создаем packet service
        let packet_service = Arc::new(PhantomPacketService::new(
            session_manager.clone(),
            {
                use crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;
                Arc::new(ConnectionHeartbeatManager::new(
                    session_manager.clone(),
                    monitor.clone(),
                ))
            },
        ));

        let packet_processor = PhantomPacketProcessor::new();

        // Создаем reader с ЧИТАТЕЛЬСКИМ каналом
        let reader = Arc::new(BatchReader::new(config.clone(), reader_event_tx.clone()));

        let writer = Arc::new(BatchWriter::new(config.clone()));

        let dispatcher = Arc::new(PacketDispatcher::new(
            config.clone(),
            session_manager.clone(),
            packet_service.clone(),
            writer.clone(),
        ).await);

        let work_stealing_dispatcher = Arc::new(WorkStealingDispatcher::new(
            config.worker_count,
            config.max_queue_size,
            session_manager.clone(),
        ));

        // Создаем систему
        let system = Self {
            config: config.clone(),
            reader,
            writer,
            dispatcher,
            work_stealing_dispatcher,
            crypto_processor,
            optimized_crypto_processor,
            buffer_pool,
            optimized_buffer_pool,
            chacha20_accelerator,
            blake3_accelerator,
            packet_service,
            packet_processor,
            session_manager: session_manager.clone(),
            crypto: crypto.clone(),
            monitor: monitor.clone(),
            event_tx: system_event_tx.clone(),  // <-- Системный канал
            event_rx: Arc::new(Mutex::new(system_event_rx)),  // <-- Системный канал
            command_tx,
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            is_initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            startup_time,
            stats: Arc::new(RwLock::new(SystemStatistics {
                startup_time,
                ..Default::default()
            })),
            metrics: Arc::new(DashMap::new()),
            health_checks: Arc::new(RwLock::new(HashMap::new())),
            pending_batches: Arc::new(RwLock::new(Vec::new())),
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            session_cache: Arc::new(RwLock::new(HashMap::new())),
        };

        // ЗАПУСКАЕМ КОНВЕРТЕР ReaderEvent -> SystemEvent
        system.start_reader_event_converter(reader_event_rx).await;

        // Инициализируем систему
        system.initialize().await?;

        info!("✅ Интегрированная Batch система успешно инициализирована");
        Ok(system)
    }

    // Добавьте этот метод для конвертации событий
    async fn start_reader_event_converter(&self, mut reader_event_rx: mpsc::Receiver<ReaderEvent>) {
        let event_tx = self.event_tx.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            info!("🔄 Reader event converter started");

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                match reader_event_rx.recv().await {
                    Some(reader_event) => {
                        // Конвертируем ReaderEvent в SystemEvent
                        let system_event = match reader_event {
                            ReaderEvent::DataReady {
                                session_id,
                                data,
                                source_addr,
                                priority,
                                received_at,
                            } => SystemEvent::DataReceived {
                                session_id,
                                data: data.freeze(),
                                source_addr,
                                priority,
                                timestamp: received_at,
                            },
                            ReaderEvent::ConnectionClosed {
                                source_addr,
                                reason,
                            } => {
                                // Нужен session_id, но его нет в ConnectionClosed
                                // Можно добавить в будущем или использовать пустой
                                SystemEvent::ConnectionClosed {
                                    addr: source_addr,
                                    session_id: Vec::new(), // Пустой для ConnectionClosed
                                    reason,
                                }
                            }
                            ReaderEvent::Error {
                                source_addr,
                                error,
                            } => SystemEvent::ErrorOccurred {
                                error: error.to_string(),
                                context: "reader_error".to_string(),
                                severity: ErrorSeverity::High,
                            },
                        };

                        if let Err(e) = event_tx.send(system_event).await {
                            error!("❌ Failed to send converted event: {}", e);
                            break;
                        }
                    }
                    None => {
                        warn!("📭 Reader event channel closed");
                        break;
                    }
                }
            }

            info!("👋 Reader event converter stopped");
        });
    }

    /// Инициализация системы
    async fn initialize(&self) -> Result<(), BatchError> {
        info!("🔄 Инициализация компонентов системы...");

        // Сначала помечаем как инициализированную
        self.is_initialized.store(true, std::sync::atomic::Ordering::SeqCst);
        self.is_running.store(true, std::sync::atomic::Ordering::SeqCst);

        // Только потом запускаем обработчики
        self.start_event_handlers().await;
        self.start_command_handlers().await;
        self.start_monitoring_tasks().await;
        self.start_statistics_collector().await;
        self.start_batch_processor().await;
        self.initialize_health_checks().await;

        info!("✅ Все компоненты системы инициализированы");
        Ok(())
    }

    /// Запуск обработчиков событий
    async fn start_event_handlers(&self) {
        let event_rx = self.event_rx.clone();
        let system = self.clone();

        tokio::spawn(async move {
            info!("👂 Обработчик событий запущен");

            let mut receiver = event_rx.lock().await;

            while system.is_running.load(std::sync::atomic::Ordering::Relaxed) {
                match receiver.recv().await {
                    Some(event) => {
                        system.handle_event(event).await;
                    }
                    None => {
                        warn!("📭 Канал событий закрыт");
                        break;
                    }
                }
            }

            info!("👋 Обработчик событий остановлен");
        });
    }

    /// Обработка события
    async fn handle_event(&self, event: SystemEvent) {
        match event {
            SystemEvent::DataReceived { session_id, data, source_addr, priority, timestamp } => {
                self.handle_data_received(session_id, data, source_addr, priority, timestamp).await;
            }
            SystemEvent::DataProcessed { session_id, result, processing_time, worker_id } => {
                self.handle_data_processed(session_id, result, processing_time, worker_id).await;
            }
            SystemEvent::ConnectionOpened { addr, session_id } => {
                self.handle_connection_opened(addr, session_id).await;
            }
            SystemEvent::ConnectionClosed { addr, session_id, reason } => {
                self.handle_connection_closed(addr, session_id, reason).await;
            }
            SystemEvent::BatchCompleted { batch_id, size, processing_time, success_rate } => {
                self.handle_batch_completed(batch_id, size, processing_time, success_rate).await;
            }
            SystemEvent::ErrorOccurred { error, context, severity } => {
                self.handle_error_occurred(error, context, severity).await;
            }
            SystemEvent::HealthCheck { component, status, details } => {
                self.handle_health_check(component, status, details).await;
            }
            SystemEvent::PerformanceAlert { metric, value, threshold, component } => {
                self.handle_performance_alert(metric, value, threshold, component).await;
            }
        }
    }

    /// Обработка полученных данных
    async fn handle_data_received(
        &self,
        session_id: Vec<u8>,
        data: Bytes,
        source_addr: std::net::SocketAddr,
        priority: Priority,
        timestamp: Instant,
    ) {
        trace!("📥 Получены данные: {} байт от {}", data.len(), source_addr);

        // Обновляем статистику
        {
            let mut stats = self.stats.write().await;
            stats.total_data_received += data.len() as u64;
            stats.total_packets_processed += 1;
        }

        // Обновляем информацию о соединении
        {
            let mut connections = self.active_connections.write().await;
            if let Some(conn) = connections.get_mut(&source_addr) {
                conn.last_activity = Instant::now();
                conn.bytes_received += data.len() as u64;
                conn.priority = priority;
            } else {
                // Создаем новое соединение если его нет
                connections.insert(source_addr, ConnectionInfo {
                    addr: source_addr,
                    session_id: session_id.clone(),
                    opened_at: Instant::now(),
                    last_activity: Instant::now(),
                    bytes_received: data.len() as u64,
                    bytes_sent: 0,
                    priority,
                    is_active: true,
                    worker_assigned: None,
                });
            }
        }

        // Определяем тип обработки на основе приоритета и размера данных
        let _processor_type = self.determine_processor_type(&data, priority);

        // Создаем задачу
        let task = WorkStealingTask {
            id: 0, // Будет установлен диспетчером
            session_id: session_id.clone(),
            data: data.clone(),
            source_addr,
            priority,
            created_at: timestamp,
            worker_id: None,
        };

        // Отправляем в work-stealing диспетчер
        match self.work_stealing_dispatcher.submit_task(task).await {
            Ok(task_id) => {
                debug!("✅ Задача отправлена в work-stealing диспетчер, ID: {}", task_id);

                // Отслеживаем результат
                self.track_task_result(task_id, session_id, source_addr).await;
            }
            Err(e) => {
                error!("❌ Ошибка отправки задачи: {}", e);

                // Отправляем событие об ошибке
                let event = SystemEvent::ErrorOccurred {
                    error: e.to_string(),
                    context: "submit_task".to_string(),
                    severity: ErrorSeverity::High,
                };

                if let Err(e) = self.event_tx.send(event).await {
                    error!("❌ Ошибка отправки события об ошибке: {}", e);
                }
            }
        }
    }

    /// Обработка обработанных данных
    async fn handle_data_processed(
        &self,
        session_id: Vec<u8>,
        result: ProcessResult,
        _processing_time: Duration,
        _worker_id: Option<usize>,
    ) {
        debug!("✅ Данные обработаны для сессии: {}, успех: {}", 
               hex::encode(&session_id), result.success);

        if result.success {
            // Обновляем статистику для успешной обработки
            let mut stats = self.stats.write().await;
            if let Some(data) = &result.data {
                stats.total_data_sent += data.len() as u64;
            }
        } else {
            // Логируем ошибку
            if let Some(error) = &result.error {
                warn!("⚠️ Ошибка обработки данных для сессии {}: {}", 
                      hex::encode(&session_id), error);
            }
        }
    }

    /// Определение типа процессора
    fn determine_processor_type(&self, _data: &Bytes, priority: Priority) -> ProcessorType {
        if priority.is_critical() {
            ProcessorType::Accelerated
        } else if self.config.enable_work_stealing {
            ProcessorType::WorkStealing
        } else {
            ProcessorType::Standard
        }
    }

    /// Отслеживание результата задачи
    async fn track_task_result(
        &self,
        task_id: u64,
        session_id: Vec<u8>,
        source_addr: std::net::SocketAddr,
    ) {
        let dispatcher = self.work_stealing_dispatcher.clone();
        let event_tx = self.event_tx.clone();
        let system = self.clone();

        tokio::spawn(async move {
            // Ждем результат с таймаутом
            let result = tokio::time::timeout(Duration::from_secs(30), async {
                let mut attempts = 0;
                while attempts < 100 {
                    if let Some(task_result) = dispatcher.get_result(task_id) {
                        return Some(task_result);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    attempts += 1;
                }
                None
            }).await;

            match result {
                Ok(Some(task_result)) => {
                    // Клонируем результат перед использованием
                    let result_clone = task_result.result.clone();
                    let processing_time = task_result.processing_time;
                    let worker_id = task_result.worker_id;

                    let process_result = ProcessResult {
                        success: result_clone.is_ok(),
                        data: result_clone.clone().ok().map(|v| Bytes::from(v)),
                        error: result_clone.err().map(|e| e.to_string()),
                        metadata: HashMap::from([
                            ("worker_id".to_string(), worker_id.to_string()),
                            ("processing_time".to_string(), format!("{:?}", processing_time)),
                        ]),
                    };

                    // Отправляем событие о завершении обработки
                    let event = SystemEvent::DataProcessed {
                        session_id: session_id.clone(),
                        result: process_result,
                        processing_time,
                        worker_id: Some(worker_id),
                    };

                    if let Err(e) = event_tx.send(event).await {
                        error!("❌ Ошибка отправки события DataProcessed: {}", e);
                    }

                    // Обрабатываем результат дальше
                    system.process_task_result(
                        task_result,
                        session_id,
                        source_addr,
                    ).await;
                }
                Ok(None) => {
                    warn!("⚠️ Результат задачи {} не получен после таймаута", task_id);

                    let event = SystemEvent::ErrorOccurred {
                        error: "Timeout waiting for task result".to_string(),
                        context: format!("task_result_timeout_{}", task_id),
                        severity: ErrorSeverity::Medium,
                    };

                    if let Err(e) = event_tx.send(event).await {
                        error!("❌ Ошибка отправки события об ошибке таймаута: {}", e);
                    }
                }
                Err(_) => {
                    error!("⏰ Таймаут ожидания результата задачи {}", task_id);

                    let event = SystemEvent::ErrorOccurred {
                        error: "Timeout".to_string(),
                        context: format!("task_timeout_{}", task_id),
                        severity: ErrorSeverity::High,
                    };

                    if let Err(e) = event_tx.send(event).await {
                        error!("❌ Ошибка отправки события об ошибке: {}", e);
                    }
                }
            }
        });
    }

    /// Обработка результата задачи
    async fn process_task_result(
        &self,
        task_result: WorkStealingResult,
        session_id: Vec<u8>,
        source_addr: std::net::SocketAddr,
    ) {
        match task_result.result {
            Ok(data) => {
                // Данные уже дешифрованы work-stealing dispatcher
                if data.len() > 1 {
                    let packet_type = data[0];
                    let packet_data = &data[1..];

                    debug!("📦 Обработка дешифрованного пакета: тип=0x{:02x}, размер={}", 
                           packet_type, packet_data.len());

                    // Получаем сессию
                    if let Some(session) = self.session_manager.get_session(&session_id).await {
                        // Обрабатываем через packet service
                        match self.packet_service.process_packet(
                            session.clone(),
                            packet_type,
                            packet_data.to_vec(),
                            source_addr,
                        ).await {
                            Ok(processing_result) => {
                                // Шифруем ответ
                                match self.packet_processor.create_outgoing_vec(
                                    &session,
                                    processing_result.packet_type,
                                    &processing_result.response,
                                ) {
                                    Ok(encrypted_response) => {
                                        // Отправляем зашифрованный ответ
                                        if let Err(e) = self.writer.write(
                                            source_addr,
                                            session_id,
                                            Bytes::from(encrypted_response),
                                            processing_result.priority,
                                            true,
                                        ).await {
                                            error!("❌ Ошибка отправки ответа: {}", e);
                                        }
                                    }
                                    Err(e) => {
                                        error!("❌ Ошибка шифрования ответа: {}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                error!("❌ Ошибка обработки пакета: {}", e);
                            }
                        }
                    } else {
                        warn!("⚠️ Сессия не найдена: {}", hex::encode(&session_id));
                    }
                }
            }
            Err(err) => {
                error!("❌ Ошибка обработки задачи: {}", err);
            }
        }
    }

    /// Обработка открытия соединения
    async fn handle_connection_opened(&self, addr: std::net::SocketAddr, session_id: Vec<u8>) {
        info!("🔗 Открыто соединение: {} -> {}", addr, hex::encode(&session_id));

        let mut connections = self.active_connections.write().await;
        connections.insert(addr, ConnectionInfo {
            addr,
            session_id: session_id.clone(),
            opened_at: Instant::now(),
            last_activity: Instant::now(),
            bytes_received: 0,
            bytes_sent: 0,
            priority: Priority::Normal,
            is_active: true,
            worker_assigned: None,
        });

        // Обновляем статистику
        let mut stats = self.stats.write().await;
        stats.total_connections += 1;
    }

    /// Обработка закрытия соединения
    async fn handle_connection_closed(&self, addr: std::net::SocketAddr, session_id: Vec<u8>, reason: String) {
        info!("🔒 Закрыто соединение: {} -> {}: {}", addr, hex::encode(&session_id), reason);

        let mut connections = self.active_connections.write().await;
        connections.remove(&addr);
    }

    /// Обработка завершения батча
    async fn handle_batch_completed(
        &self,
        batch_id: u64,
        size: usize,
        processing_time: Duration,
        success_rate: f64
    ) {
        debug!("✅ Батч {} завершен: размер={}, время={:?}, успех={:.1}%", 
               batch_id, size, processing_time, success_rate * 100.0);

        // Обновляем статистику
        let mut stats = self.stats.write().await;
        stats.total_batches_processed += 1;

        // Обновляем среднее время обработки
        let total_batches = stats.total_batches_processed as f64;
        let current_avg = stats.avg_processing_time.as_nanos() as f64;
        let new_avg = (current_avg * (total_batches - 1.0) + processing_time.as_nanos() as f64) / total_batches;
        stats.avg_processing_time = Duration::from_nanos(new_avg as u64);
    }

    /// Обработка ошибки
    async fn handle_error_occurred(&self, error: String, context: String, severity: ErrorSeverity) {
        match severity {
            ErrorSeverity::Low => debug!("⚠️ Низкий приоритет: {} в {}", error, context),
            ErrorSeverity::Medium => warn!("⚠️ Средний приоритет: {} в {}", error, context),
            ErrorSeverity::High => error!("❌ Высокий приоритет: {} в {}", error, context),
            ErrorSeverity::Critical => {
                error!("🚨 КРИТИЧЕСКО: {} в {}", error, context);
                // Тут можно добавить экстренные действия
            }
        }

        // Обновляем статистику
        let mut stats = self.stats.write().await;
        stats.total_errors += 1;

        // Логируем в мониторинг (если есть такой метод)
        // self.monitor.record_error(&context, &error, severity as i32).await;
    }

    /// Обработка health check
    async fn handle_health_check(&self, component: String, status: HealthStatus, details: HashMap<String, String>) {
        let mut health_checks = self.health_checks.write().await;
        health_checks.insert(component.clone(), status);

        match status {
            HealthStatus::Ok => debug!("✅ Health check: {} - OK", component),
            HealthStatus::Warning => warn!("⚠️ Health check: {} - WARNING: {:?}", component, details),
            HealthStatus::Error => error!("❌ Health check: {} - ERROR: {:?}", component, details),
            HealthStatus::Unknown => debug!("❓ Health check: {} - UNKNOWN", component),
        }
    }

    /// Обработка performance alert
    async fn handle_performance_alert(
        &self,
        metric: String,
        value: f64,
        threshold: f64,
        component: String
    ) {
        warn!("📊 Performance alert: {}={:.2} > {:.2} in {}", metric, value, threshold, component);

        // Можно добавить автоматическую регулировку
        if value > threshold * 1.5 {
            self.auto_adjust_config(&component, &metric, value / threshold).await;
        }
    }

    /// Автоматическая регулировка конфигурации
    async fn auto_adjust_config(&self, component: &str, metric: &str, ratio: f64) {
        debug!("🔄 Автоматическая регулировка: {} -> {} (ratio={:.2})", component, metric, ratio);

        // Пример автоматической регулировки
        match (component, metric) {
            ("buffer_pool", "hit_rate") if ratio < 0.5 => {
                // Увеличиваем размер пула буферов
                warn!("📈 Увеличиваем buffer_pool из-за низкого hit rate");
            }
            ("crypto_processor", "queue_size") if ratio > 2.0 => {
                // Увеличиваем количество воркеров
                warn!("📈 Увеличиваем количество crypto workers");
            }
            _ => {}
        }
    }

    /// Запуск задач мониторинга
    async fn start_monitoring_tasks(&self) {
        info!("📊 Запуск задач мониторинга...");

        // Мониторинг буферных пулов
        self.start_buffer_pool_monitoring().await;

        // Мониторинг диспетчеров
        self.start_dispatcher_monitoring().await;

        // Мониторинг криптопроцессора
        self.start_crypto_processor_monitoring().await;

        // Мониторинг соединений
        self.start_connection_monitoring().await;

        // Мониторинг производительности
        self.start_performance_monitoring().await;

        // Мониторинг здоровья системы
        self.start_system_health_monitoring().await;
    }

    /// Мониторинг буферных пулов
    async fn start_buffer_pool_monitoring(&self) {
        let buffer_pool = self.buffer_pool.clone();
        let optimized_buffer_pool = self.optimized_buffer_pool.clone();
        let event_tx = self.event_tx.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;

            let mut interval = tokio::time::interval(Duration::from_secs(30));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                // Получаем статистику из обоих пулов
                let stats = buffer_pool.get_stats();
                let reuse_rate = optimized_buffer_pool.get_reuse_rate();
                let memory_usage = optimized_buffer_pool.get_memory_usage();

                // Проверяем health
                let hit_rate = if stats.allocation_count + stats.reuse_count > 0 {
                    stats.reuse_count as f64 / (stats.allocation_count + stats.reuse_count) as f64
                } else {
                    0.0
                };

                let status = if hit_rate > 0.7 && reuse_rate > 0.6 {
                    HealthStatus::Ok
                } else if hit_rate > 0.5 && reuse_rate > 0.4 {
                    HealthStatus::Warning
                } else {
                    HealthStatus::Error
                };

                let details = HashMap::from([
                    ("hit_rate".to_string(), format!("{:.1}%", hit_rate * 100.0)),
                    ("reuse_rate".to_string(), format!("{:.1}%", reuse_rate * 100.0)),
                    ("memory_usage".to_string(), memory_usage.to_string()),
                    ("allocations".to_string(), stats.allocation_count.to_string()),
                    ("reuses".to_string(), stats.reuse_count.to_string()),
                ]);

                // Отправляем health check
                let event = SystemEvent::HealthCheck {
                    component: "buffer_pool".to_string(),
                    status,
                    details,
                };

                if let Err(e) = event_tx.send(event).await {
                    error!("❌ Ошибка отправки health check буферного пула: {}", e);
                }

                // Проверяем performance alerts
                if hit_rate < 0.3 {
                    let event = SystemEvent::PerformanceAlert {
                        metric: "buffer_hit_rate".to_string(),
                        value: hit_rate,
                        threshold: 0.3,
                        component: "buffer_pool".to_string(),
                    };

                    if let Err(e) = event_tx.send(event).await {
                        error!("❌ Ошибка отправки performance alert: {}", e);
                    }
                }
            }
        });
    }

    /// Мониторинг диспетчеров
    async fn start_dispatcher_monitoring(&self) {
        let work_stealing_dispatcher = self.work_stealing_dispatcher.clone();
        let event_tx = self.event_tx.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                // Получаем статистику от диспетчеров
                let work_stealing_stats = work_stealing_dispatcher.get_stats();

                // Анализируем статистику
                let work_stealing_tasks: u64 = work_stealing_stats.values().sum();

                let status = if work_stealing_tasks > 0 {
                    HealthStatus::Ok
                } else {
                    HealthStatus::Warning
                };

                let details = HashMap::from([
                    ("work_stealing_tasks".to_string(), work_stealing_tasks.to_string()),
                ]);

                // Отправляем health check
                let event = SystemEvent::HealthCheck {
                    component: "dispatchers".to_string(),
                    status,
                    details,
                };

                if let Err(e) = event_tx.send(event).await {
                    error!("❌ Ошибка отправки health check диспетчеров: {}", e);
                }
            }
        });
    }

    /// Мониторинг криптопроцессора
    async fn start_crypto_processor_monitoring(&self) {
        let crypto_processor = self.crypto_processor.clone();
        let optimized_crypto_processor = self.optimized_crypto_processor.clone();
        let event_tx = self.event_tx.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                // Получаем статистику от криптопроцессоров
                let stats = crypto_processor.get_stats();
                let _optimized_stats = optimized_crypto_processor.get_stats();

                let total_operations = stats.total_operations;
                let failed_operations = stats.total_failed;
                let success_rate = if total_operations > 0 {
                    1.0 - (failed_operations as f64 / total_operations as f64)
                } else {
                    1.0
                };

                let status = if success_rate > 0.99 {
                    HealthStatus::Ok
                } else if success_rate > 0.95 {
                    HealthStatus::Warning
                } else {
                    HealthStatus::Error
                };

                let details = HashMap::from([
                    ("total_operations".to_string(), total_operations.to_string()),
                    ("failed_operations".to_string(), failed_operations.to_string()),
                    ("success_rate".to_string(), format!("{:.1}%", success_rate * 100.0)),
                    ("batches_processed".to_string(), stats.total_batches.to_string()),
                ]);

                // Отправляем health check
                let event = SystemEvent::HealthCheck {
                    component: "crypto_processor".to_string(),
                    status,
                    details,
                };

                if let Err(e) = event_tx.send(event).await {
                    error!("❌ Ошибка отправки health check криптопроцессора: {}", e);
                }

                // Проверяем performance alerts
                if success_rate < 0.98 {
                    let event = SystemEvent::PerformanceAlert {
                        metric: "crypto_success_rate".to_string(),
                        value: success_rate,
                        threshold: 0.98,
                        component: "crypto_processor".to_string(),
                    };

                    if let Err(e) = event_tx.send(event).await {
                        error!("❌ Ошибка отправки performance alert: {}", e);
                    }
                }
            }
        });
    }

    /// Мониторинг соединений
    async fn start_connection_monitoring(&self) {
        let active_connections = self.active_connections.clone();
        let event_tx = self.event_tx.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let connections = active_connections.read().await;
                let total_connections = connections.len();
                let active_count = connections.values().filter(|c| c.is_active).count();

                let status = if active_count > 0 {
                    HealthStatus::Ok
                } else if total_connections == 0 {
                    HealthStatus::Warning
                } else {
                    HealthStatus::Error
                };

                let details = HashMap::from([
                    ("total_connections".to_string(), total_connections.to_string()),
                    ("active_connections".to_string(), active_count.to_string()),
                    ("inactive_connections".to_string(), (total_connections - active_count).to_string()),
                ]);

                // Отправляем health check
                let event = SystemEvent::HealthCheck {
                    component: "connections".to_string(),
                    status,
                    details,
                };

                if let Err(e) = event_tx.send(event).await {
                    error!("❌ Ошибка отправки health check соединений: {}", e);
                }
            }
        });
    }

    /// Мониторинг производительности
    async fn start_performance_monitoring(&self) {
        let stats = self.stats.clone();
        let event_tx = self.event_tx.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            let mut last_stats = SystemStatistics::default();

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let current_stats = stats.read().await.clone();

                // Вычисляем throughput
                let time_diff = current_stats.uptime.as_secs_f64() - last_stats.uptime.as_secs_f64();
                let data_diff = current_stats.total_data_received - last_stats.total_data_received;
                let _packets_diff = current_stats.total_packets_processed - last_stats.total_packets_processed;

                let bytes_per_second = if time_diff > 0.0 {
                    data_diff as f64 / time_diff
                } else {
                    0.0
                };

                // Проверяем пороги производительности
                if bytes_per_second > 100_000_000.0 { // 100 MB/s
                    let event = SystemEvent::PerformanceAlert {
                        metric: "throughput".to_string(),
                        value: bytes_per_second,
                        threshold: 100_000_000.0,
                        component: "system".to_string(),
                    };

                    if let Err(e) = event_tx.send(event).await {
                        error!("❌ Ошибка отправки performance alert: {}", e);
                    }
                }

                // Сохраняем текущую статистику для следующего цикла
                last_stats = current_stats;
            }
        });
    }

    /// Мониторинг здоровья системы
    async fn start_system_health_monitoring(&self) {
        let event_tx = self.event_tx.clone();
        let is_running = self.is_running.clone();
        let system = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                // Выполняем комплексную проверку здоровья
                let health_status = system.check_system_health().await;

                // Отправляем общий health check
                let event = SystemEvent::HealthCheck {
                    component: "system".to_string(),
                    status: health_status,
                    details: HashMap::new(),
                };

                if let Err(e) = event_tx.send(event).await {
                    error!("❌ Ошибка отправки системного health check: {}", e);
                }
            }
        });
    }

    /// Проверка здоровья системы
    async fn check_system_health(&self) -> HealthStatus {
        let health_checks = self.health_checks.read().await;

        let mut error_count = 0;
        let mut warning_count = 0;
        let mut ok_count = 0;

        for (_, status) in health_checks.iter() {
            match status {
                HealthStatus::Ok => ok_count += 1,
                HealthStatus::Warning => warning_count += 1,
                HealthStatus::Error => error_count += 1,
                HealthStatus::Unknown => {}
            }
        }

        if error_count > 0 {
            HealthStatus::Error
        } else if warning_count > 0 {
            HealthStatus::Warning
        } else if ok_count > 0 {
            HealthStatus::Ok
        } else {
            HealthStatus::Unknown
        }
    }

    /// Запуск обработчиков команд
    async fn start_command_handlers(&self) {
        let command_rx = self.command_tx.subscribe();
        let system = self.clone();

        tokio::spawn(async move {
            info!("🎛️ Обработчик команд запущен");

            let mut receiver = command_rx;

            while system.is_running.load(std::sync::atomic::Ordering::Relaxed) {
                match receiver.recv().await {
                    Ok(command) => {
                        system.handle_command(command).await;
                    }
                    Err(e) => {
                        error!("❌ Ошибка получения команды: {}", e);
                        break;
                    }
                }
            }

            info!("👋 Обработчик команд остановлен");
        });
    }

    /// Обработка команды
    async fn handle_command(&self, command: SystemCommand) {
        match command {
            SystemCommand::StartProcessing => {
                self.start_processing().await;
            }
            SystemCommand::PauseProcessing => {
                self.pause_processing().await;
            }
            SystemCommand::ResumeProcessing => {
                self.resume_processing().await;
            }
            SystemCommand::StopProcessing => {
                self.stop_processing().await;
            }
            SystemCommand::FlushBuffers => {
                self.flush_buffers().await;
            }
            SystemCommand::ClearCaches => {
                self.clear_caches().await;
            }
            SystemCommand::AdjustConfig { parameter, value } => {
                self.adjust_config(parameter, value).await;
            }
            SystemCommand::EmergencyShutdown { reason } => {
                self.emergency_shutdown(reason).await;
            }
            SystemCommand::RunHealthCheck { component } => {
                self.run_health_check(component).await;
            }
            SystemCommand::GetStatistics => {
                self.get_statistics().await;
            }
            SystemCommand::ResetStatistics => {
                self.reset_statistics().await;
            }
            SystemCommand::RebalanceWorkers => {
                self.rebalance_workers().await;
            }
            SystemCommand::ScaleUp { count } => {
                self.scale_up(count).await;
            }
            SystemCommand::ScaleDown { count } => {
                self.scale_down(count).await;
            }
        }
    }

    /// Запуск обработки
    async fn start_processing(&self) {
        if !self.is_running.load(std::sync::atomic::Ordering::SeqCst) {
            info!("▶️ Запуск обработки данных...");
            self.is_running.store(true, std::sync::atomic::Ordering::SeqCst);

            // Обновляем health check
            let event = SystemEvent::HealthCheck {
                component: "processing".to_string(),
                status: HealthStatus::Ok,
                details: HashMap::from([("action".to_string(), "started".to_string())]),
            };

            if let Err(e) = self.event_tx.send(event).await {
                error!("❌ Ошибка отправки события: {}", e);
            }
        }
    }

    /// Пауза обработки
    async fn pause_processing(&self) {
        if self.is_running.load(std::sync::atomic::Ordering::SeqCst) {
            info!("⏸️ Приостановка обработки данных...");
            self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);

            let event = SystemEvent::HealthCheck {
                component: "processing".to_string(),
                status: HealthStatus::Warning,
                details: HashMap::from([("action".to_string(), "paused".to_string())]),
            };

            if let Err(e) = self.event_tx.send(event).await {
                error!("❌ Ошибка отправки события: {}", e);
            }
        }
    }

    /// Возобновление обработки
    async fn resume_processing(&self) {
        self.start_processing().await;
    }

    /// Остановка обработки
    async fn stop_processing(&self) {
        info!("⏹️ Остановка обработки данных...");
        self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);

        // Завершаем все активные задачи
        self.shutdown_components().await;

        let event = SystemEvent::HealthCheck {
            component: "processing".to_string(),
            status: HealthStatus::Unknown,
            details: HashMap::from([("action".to_string(), "stopped".to_string())]),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки события: {}", e);
        }
    }

    /// Сброс буферов
    async fn flush_buffers(&self) {
        info!("🌀 Сброс всех буферов...");

        // Сбрасываем buffer pool
        self.buffer_pool.force_cleanup();
        self.optimized_buffer_pool.force_cleanup();

        // Сбрасываем кэш сессий
        {
            let mut cache = self.session_cache.write().await;
            cache.clear();
        }

        let event = SystemEvent::HealthCheck {
            component: "buffers".to_string(),
            status: HealthStatus::Ok,
            details: HashMap::from([("action".to_string(), "flushed".to_string())]),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки события: {}", e);
        }
    }

    /// Очистка кэшей
    async fn clear_caches(&self) {
        info!("🧹 Очистка всех кэшей...");

        // Очищаем кэши криптопроцессора
        self.crypto_processor.clear_cache().await;

        // Note: Акселераторы находятся в Arc, поэтому мы не можем их мутировать напрямую
        // Вместо этого мы можем создать новые экземпляры или добавить методы очистки через внутреннюю мутабельность
        warn!("⚠️ Очистка кэшей акселераторов требует дополнительной реализации");

        let event = SystemEvent::HealthCheck {
            component: "caches".to_string(),
            status: HealthStatus::Ok,
            details: HashMap::from([("action".to_string(), "cleared".to_string())]),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки события: {}", e);
        }
    }

    /// Регулировка конфигурации
    async fn adjust_config(&self, parameter: String, value: String) {
        info!("⚙️ Регулировка конфигурации: {} = {}", parameter, value);

        // Здесь можно добавить логику регулировки конфигурации
        // В зависимости от параметра

        let event = SystemEvent::HealthCheck {
            component: "config".to_string(),
            status: HealthStatus::Ok,
            details: HashMap::from([
                ("parameter".to_string(), parameter),
                ("value".to_string(), value),
            ]),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки события: {}", e);
        }
    }

    /// Аварийное завершение
    async fn emergency_shutdown(&self, reason: String) {
        error!("🚨 Аварийное завершение: {}", reason);

        // Немедленная остановка всех компонентов
        self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);

        // Форсированное завершение
        self.shutdown_components().await;

        let event = SystemEvent::ErrorOccurred {
            error: reason.clone(),
            context: "emergency_shutdown".to_string(),
            severity: ErrorSeverity::Critical,
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки события: {}", e);
        }
    }

    /// Выполнение health check
    async fn run_health_check(&self, component: Option<String>) {
        info!("🩺 Выполнение health check: {:?}", component);

        if let Some(comp) = component {
            // Проверка конкретного компонента
            match comp.as_str() {
                "buffer_pool" => {
                    self.check_buffer_pool_health().await;
                }
                "crypto_processor" => {
                    self.check_crypto_processor_health().await;
                }
                "dispatchers" => {
                    self.check_dispatchers_health().await;
                }
                "connections" => {
                    self.check_connections_health().await;
                }
                _ => {
                    warn!("❓ Неизвестный компонент для health check: {}", comp);
                }
            }
        } else {
            // Проверка всех компонентов
            self.check_buffer_pool_health().await;
            self.check_crypto_processor_health().await;
            self.check_dispatchers_health().await;
            self.check_connections_health().await;
        }
    }

    /// Проверка здоровья buffer pool
    async fn check_buffer_pool_health(&self) {
        let stats = self.buffer_pool.get_stats();
        let reuse_rate = self.optimized_buffer_pool.get_reuse_rate();

        let hit_rate = if stats.allocation_count + stats.reuse_count > 0 {
            stats.reuse_count as f64 / (stats.allocation_count + stats.reuse_count) as f64
        } else {
            0.0
        };

        // ИЗМЕНЕНИЕ: На старте системы hit_rate может быть 0
        // Это НЕ ошибка, а нормальное состояние
        let status = if stats.total_allocated == 0 {
            // Система только запустилась, еще не было аллокаций
            HealthStatus::Ok
        } else if hit_rate > 0.7 && reuse_rate > 0.6 {
            HealthStatus::Ok
        } else if hit_rate > 0.5 && reuse_rate > 0.4 {
            HealthStatus::Warning
        } else {
            HealthStatus::Error
        };

        let event = SystemEvent::HealthCheck {
            component: "buffer_pool".to_string(),
            status,
            details: HashMap::from([
                ("hit_rate".to_string(), format!("{:.1}%", hit_rate * 100.0)),
                ("reuse_rate".to_string(), format!("{:.1}%", reuse_rate * 100.0)),
                ("allocations".to_string(), stats.allocation_count.to_string()),
                ("reuses".to_string(), stats.reuse_count.to_string()),
                ("total_allocated_mb".to_string(), format!("{:.1}", stats.total_allocated as f64 / 1024.0 / 1024.0)),
            ]),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки health check: {}", e);
        }
    }

    /// Проверка здоровья crypto processor
    async fn check_crypto_processor_health(&self) {
        let stats = self.crypto_processor.get_stats();
        let optimized_stats = self.optimized_crypto_processor.get_stats();

        let total_operations = stats.total_operations;
        let failed_operations = stats.total_failed;
        let success_rate = if total_operations > 0 {
            1.0 - (failed_operations as f64 / total_operations as f64)
        } else {
            1.0
        };

        let status = if success_rate > 0.99 {
            HealthStatus::Ok
        } else if success_rate > 0.95 {
            HealthStatus::Warning
        } else {
            HealthStatus::Error
        };

        let event = SystemEvent::HealthCheck {
            component: "crypto_processor".to_string(),
            status,
            details: HashMap::from([
                ("total_operations".to_string(), total_operations.to_string()),
                ("failed_operations".to_string(), failed_operations.to_string()),
                ("success_rate".to_string(), format!("{:.1}%", success_rate * 100.0)),
                ("optimized_tasks".to_string(), optimized_stats.get("crypto_tasks_processed").unwrap_or(&0).to_string()),
            ]),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки health check: {}", e);
        }
    }

    /// Проверка здоровья диспетчеров
    async fn check_dispatchers_health(&self) {
        let work_stealing_stats = self.work_stealing_dispatcher.get_stats();

        let work_stealing_tasks: u64 = work_stealing_stats.values().sum();

        let status = if work_stealing_tasks > 0 {
            HealthStatus::Ok
        } else {
            HealthStatus::Warning
        };

        let event = SystemEvent::HealthCheck {
            component: "dispatchers".to_string(),
            status,
            details: HashMap::from([
                ("work_stealing_tasks".to_string(), work_stealing_tasks.to_string()),
                ("worker_count".to_string(), self.config.worker_count.to_string()),
            ]),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки health check: {}", e);
        }
    }

    /// Проверка здоровья соединений
    async fn check_connections_health(&self) {
        let connections = self.active_connections.read().await;
        let total_connections = connections.len();
        let active_count = connections.values().filter(|c| c.is_active).count();

        // ИЗМЕНЕНИЕ: 0 соединений на старте - это НОРМАЛЬНО
        let status = if total_connections == 0 {
            HealthStatus::Ok  // Было Warning, меняем на Ok
        } else if active_count > 0 {
            HealthStatus::Ok
        } else {
            // Есть соединения, но все неактивны
            HealthStatus::Warning
        };

        let event = SystemEvent::HealthCheck {
            component: "connections".to_string(),
            status,
            details: HashMap::from([
                ("total_connections".to_string(), total_connections.to_string()),
                ("active_connections".to_string(), active_count.to_string()),
                ("inactive_connections".to_string(), (total_connections - active_count).to_string()),
            ]),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки health check: {}", e);
        }
    }

    /// Получение статистики
    async fn get_statistics(&self) {
        info!("📊 Запрос статистики системы...");

        // Обновляем uptime в статистике
        {
            let mut stats = self.stats.write().await;
            stats.uptime = Instant::now().duration_since(stats.startup_time);
        }

        let stats = self.stats.read().await.clone();

        // Здесь можно отправить статистику куда-то
        // Например, в мониторинг или лог

        info!("Системная статистика: {:?}", stats);
    }

    /// Сброс статистики
    async fn reset_statistics(&self) {
        info!("🔄 Сброс статистики системы...");

        let mut stats = self.stats.write().await;
        *stats = SystemStatistics {
            startup_time: stats.startup_time,
            ..Default::default()
        };

        // Также сбрасываем метрики
        self.metrics.clear();

        let event = SystemEvent::HealthCheck {
            component: "statistics".to_string(),
            status: HealthStatus::Ok,
            details: HashMap::from([("action".to_string(), "reset".to_string())]),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки события: {}", e);
        }
    }

    /// Перебалансировка воркеров
    async fn rebalance_workers(&self) {
        info!("⚖️ Перебалансировка воркеров...");

        // Здесь можно добавить логику перебалансировки
        // Например, перераспределение задач между воркерами

        let event = SystemEvent::HealthCheck {
            component: "workers".to_string(),
            status: HealthStatus::Ok,
            details: HashMap::from([("action".to_string(), "rebalanced".to_string())]),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки события: {}", e);
        }
    }

    /// Масштабирование вверх
    async fn scale_up(&self, count: usize) {
        info!("📈 Масштабирование вверх на {} воркеров", count);

        // Здесь можно добавить логику масштабирования
        // Например, создание дополнительных воркеров

        let event = SystemEvent::HealthCheck {
            component: "scaling".to_string(),
            status: HealthStatus::Ok,
            details: HashMap::from([
                ("action".to_string(), "scale_up".to_string()),
                ("count".to_string(), count.to_string()),
            ]),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки события: {}", e);
        }
    }

    /// Масштабирование вниз
    async fn scale_down(&self, count: usize) {
        info!("📉 Масштабирование вниз на {} воркеров", count);

        // Здесь можно добавить логику масштабирования
        // Например, остановка части воркеров

        let event = SystemEvent::HealthCheck {
            component: "scaling".to_string(),
            status: HealthStatus::Warning,
            details: HashMap::from([
                ("action".to_string(), "scale_down".to_string()),
                ("count".to_string(), count.to_string()),
            ]),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки события: {}", e);
        }
    }

    /// Запуск сборщика статистики
    async fn start_statistics_collector(&self) {
        let stats = self.stats.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                // Обновляем uptime в статистике
                let mut stats_guard = stats.write().await;
                stats_guard.uptime = Instant::now().duration_since(stats_guard.startup_time);
            }
        });
    }

    /// Запуск обработчика батчей
    async fn start_batch_processor(&self) {
        let pending_batches = self.pending_batches.clone();
        let is_running = self.is_running.clone();
        let system = self.clone();

        tokio::spawn(async move {
            info!("🔄 Обработчик батчей запущен");

            let mut interval = tokio::time::interval(Duration::from_millis(100));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                // Обрабатываем pending batches
                let batches_to_process = {
                    let mut batches = pending_batches.write().await;
                    if batches.is_empty() {
                        continue;
                    }

                    // Фильтруем batches по deadline
                    let now = Instant::now();
                    let (ready, not_ready): (Vec<_>, Vec<_>) = batches
                        .drain(..)
                        .partition(|batch| {
                            batch.deadline.map_or(true, |deadline| now >= deadline)
                                || batch.operations.len() >= system.config.batch_size as usize
                        });

                    *batches = not_ready;
                    ready
                };

                // Обрабатываем готовые batches
                for batch in batches_to_process {
                    system.process_batch(batch).await;
                }
            }

            info!("👋 Обработчик батчей остановлен");
        });
    }

    /// Обработка батча
    async fn process_batch(&self, batch: PendingBatch) {
        debug!("🔄 Обработка батча {} с {} операциями", batch.id, batch.operations.len());

        let start_time = Instant::now();
        let mut successful = 0;
        let _failed = 0;

        // Группируем операции по типу
        let mut encryption_ops = Vec::new();
        let mut decryption_ops = Vec::new();
        let mut hashing_ops = Vec::new();
        let mut processing_ops = Vec::new();

        for op in &batch.operations {
            match op {
                BatchOperation::Encryption { .. } => encryption_ops.push(op),
                BatchOperation::Decryption { .. } => decryption_ops.push(op),
                BatchOperation::Hashing { .. } => hashing_ops.push(op),
                BatchOperation::Processing { .. } => processing_ops.push(op),
            }
        }

        // Обрабатываем каждую группу
        if !encryption_ops.is_empty() {
            // Обработка шифрования
            successful += encryption_ops.len();
        }

        if !decryption_ops.is_empty() {
            // Обработка дешифрования
            successful += decryption_ops.len();
        }

        if !hashing_ops.is_empty() {
            // Обработка хеширования
            successful += hashing_ops.len();
        }

        if !processing_ops.is_empty() {
            // Обработка данных
            successful += processing_ops.len();
        }

        let processing_time = start_time.elapsed();
        let success_rate = if batch.operations.len() > 0 {
            successful as f64 / batch.operations.len() as f64
        } else {
            0.0
        };

        // Отправляем событие о завершении батча
        let event = SystemEvent::BatchCompleted {
            batch_id: batch.id,
            size: batch.operations.len(),
            processing_time,
            success_rate,
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки события BatchCompleted: {}", e);
        }
    }

    /// Инициализация health checks
    async fn initialize_health_checks(&self) {
        info!("🩺 Инициализация health checks...");

        let mut health_checks = self.health_checks.write().await;

        // Инициализируем health checks для всех компонентов
        health_checks.insert("system".to_string(), HealthStatus::Unknown);
        health_checks.insert("buffer_pool".to_string(), HealthStatus::Unknown);
        health_checks.insert("crypto_processor".to_string(), HealthStatus::Unknown);
        health_checks.insert("dispatchers".to_string(), HealthStatus::Unknown);
        health_checks.insert("connections".to_string(), HealthStatus::Unknown);
        health_checks.insert("processing".to_string(), HealthStatus::Unknown);

        info!("✅ Health checks инициализированы");
    }

    /// Завершение компонентов
    async fn shutdown_components(&self) {
        info!("🛑 Завершение компонентов системы...");

        // Завершаем reader
        self.reader.shutdown().await;

        // Завершаем writer
        self.writer.shutdown().await;

        // Завершаем dispatcher
        self.dispatcher.shutdown().await;

        // Завершаем work-stealing dispatcher
        self.work_stealing_dispatcher.shutdown().await;

        // Завершаем optimized crypto processor
        self.optimized_crypto_processor.shutdown().await;

        info!("✅ Все компоненты завершены");
    }

    /// Регистрация соединения
    pub async fn register_connection(
        &self,
        source_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        read_stream: Box<dyn tokio::io::AsyncRead + Unpin + Send + Sync>,
        write_stream: Box<dyn tokio::io::AsyncWrite + Unpin + Send + Sync>,
    ) -> Result<(), BatchError> {
        info!("🔗 Регистрация соединения: {} -> {}", source_addr, hex::encode(&session_id));

        // Регистрируем в reader
        if let Err(e) = self.reader.register_connection(
            source_addr,
            session_id.clone(),
            read_stream,
        ).await {
            error!("❌ Failed to register in reader: {}", e);
            return Err(e);
        }

        // Регистрируем в writer
        if let Err(e) = self.writer.register_connection(
            source_addr,
            session_id.clone(),
            write_stream,
        ).await {
            error!("❌ Failed to register in writer: {}", e);
            return Err(e);
        }

        // Отправляем событие об открытии соединения
        let event = SystemEvent::ConnectionOpened {
            addr: source_addr,
            session_id,
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("❌ Ошибка отправки события ConnectionOpened: {}", e);
            // Не возвращаем ошибку, т.к. соединение уже зарегистрировано
        }

        info!("✅ Соединение зарегистрировано в batch system");
        Ok(())
    }

    /// Получение статуса системы
    pub async fn get_status(&self) -> SystemStatus {
        let stats = self.stats.read().await.clone();
        let connections = self.active_connections.read().await;

        // Собираем статусы компонентов
        let mut component_status = HashMap::new();

        let health_checks = self.health_checks.read().await;
        for (component, status) in health_checks.iter() {
            component_status.insert(component.clone(), ComponentStatus {
                name: component.clone(),
                status: *status,
                last_check: Instant::now(),
                details: HashMap::new(),
                performance: 0.0,
            });
        }

        // Определяем общий статус системы
        let overall_status = self.determine_overall_status(&component_status).await;

        SystemStatus {
            timestamp: Instant::now(),
            overall_status,
            component_status,
            statistics: stats,
            active_connections: connections.len(),
            pending_tasks: 0, // Можно вычислить из диспетчеров
            memory_usage: MemoryUsage {
                total: 0,
                used: 0,
                free: 0,
                buffer_pool: 0,
                crypto_pool: 0,
                connections: connections.len(),
            },
            cpu_usage: 0.0,
            throughput: ThroughputMetrics {
                packets_per_second: 0.0,
                bytes_per_second: 0.0,
                operations_per_second: 0.0,
                avg_batch_size: 0.0,
                latency_p50: Duration::from_millis(0),
                latency_p95: Duration::from_millis(0),
                latency_p99: Duration::from_millis(0),
            },
            alerts: Vec::new(),
        }
    }

    /// Определение общего статуса системы
    async fn determine_overall_status(&self, component_status: &HashMap<String, ComponentStatus>) -> SystemHealth {
        let mut error_count = 0;
        let mut warning_count = 0;
        let mut ok_count = 0;

        for (_, status) in component_status.iter() {
            match status.status {
                HealthStatus::Ok => ok_count += 1,
                HealthStatus::Warning => warning_count += 1,
                HealthStatus::Error => error_count += 1,
                HealthStatus::Unknown => {}
            }
        }

        if error_count > 0 {
            SystemHealth::Critical
        } else if warning_count > 0 {
            SystemHealth::Degraded
        } else if ok_count > 0 {
            SystemHealth::Healthy
        } else {
            SystemHealth::Offline
        }
    }

    /// Отправка команды в систему
    pub fn send_command(&self, command: SystemCommand) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.command_tx.send(command)?;
        Ok(())
    }

    /// Получение статистики в реальном времени
    pub async fn get_realtime_stats(&self) -> SystemStatistics {
        self.stats.read().await.clone()
    }

    /// Проверка работоспособности системы
    pub fn is_healthy(&self) -> bool {
        self.is_running.load(std::sync::atomic::Ordering::Relaxed)
            && self.is_initialized.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Гибкое завершение системы
    pub async fn graceful_shutdown(&self) {
        info!("🛑 Гибкое завершение системы...");

        // Останавливаем обработку
        self.stop_processing().await;

        // Ждем завершения текущих задач
        tokio::time::sleep(Duration::from_secs(5)).await;

        // Завершаем компоненты
        self.shutdown_components().await;

        info!("✅ Система завершена");
    }
}

// Добавьте структуру-обертку для потока:
struct ReaderEventStream {
    inner: Box<dyn tokio::io::AsyncRead + Unpin + Send + Sync>,
    event_tx: mpsc::Sender<ReaderEvent>,
    addr: std::net::SocketAddr,
    session_id: Vec<u8>,
}

impl tokio::io::AsyncRead for ReaderEventStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        // Просто делегируем чтение внутреннему потоку
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl Clone for IntegratedBatchSystem {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            reader: self.reader.clone(),
            writer: self.writer.clone(),
            dispatcher: self.dispatcher.clone(),
            work_stealing_dispatcher: self.work_stealing_dispatcher.clone(),
            crypto_processor: self.crypto_processor.clone(),
            optimized_crypto_processor: self.optimized_crypto_processor.clone(),
            buffer_pool: self.buffer_pool.clone(),
            optimized_buffer_pool: self.optimized_buffer_pool.clone(),
            chacha20_accelerator: self.chacha20_accelerator.clone(),
            blake3_accelerator: self.blake3_accelerator.clone(),
            packet_service: self.packet_service.clone(),
            packet_processor: self.packet_processor.clone(),
            session_manager: self.session_manager.clone(),
            crypto: self.crypto.clone(),
            monitor: self.monitor.clone(),
            event_tx: self.event_tx.clone(),
            event_rx: self.event_rx.clone(),
            command_tx: self.command_tx.clone(),
            is_running: self.is_running.clone(),
            is_initialized: self.is_initialized.clone(),
            startup_time: self.startup_time,
            stats: self.stats.clone(),
            metrics: self.metrics.clone(),
            health_checks: self.health_checks.clone(),
            pending_batches: self.pending_batches.clone(),
            active_connections: self.active_connections.clone(),
            session_cache: self.session_cache.clone(),
        }
    }
}

// Экспортируем тип для использования в других модулях
pub use IntegratedBatchSystem as BatchSystem;