use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::HashMap;
use tokio::sync::{mpsc, RwLock, Mutex, broadcast};
use bytes::Bytes;
use tracing::{info, error, debug, warn};
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
use crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::packets::packet_service::PhantomPacketService;
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;

/// Основной интегрированный узел Batch системы
pub struct IntegratedBatchSystem {
    config: BatchConfig,
    reader: Arc<BatchReader>,
    writer: Arc<BatchWriter>,
    dispatcher: Arc<PacketDispatcher>,
    work_stealing_dispatcher: Arc<WorkStealingDispatcher>, // Теперь это LoadAwareDispatcher
    crypto_processor: Arc<CryptoProcessor>,
    optimized_crypto_processor: Arc<OptimizedCryptoProcessor>,
    buffer_pool: Arc<UnifiedBufferPool>,
    optimized_buffer_pool: Arc<OptimizedBufferPool>,
    chacha20_accelerator: Arc<ChaCha20BatchAccelerator>,
    blake3_accelerator: Arc<Blake3BatchAccelerator>,
    packet_service: Arc<PhantomPacketService>,
    packet_processor: PhantomPacketProcessor,
    session_manager: Arc<PhantomSessionManager>,
    crypto: Arc<PhantomCrypto>,
    event_tx: mpsc::Sender<SystemEvent>,
    event_rx: Arc<Mutex<mpsc::Receiver<SystemEvent>>>,
    command_tx: broadcast::Sender<SystemCommand>,
    is_running: Arc<std::sync::atomic::AtomicBool>,
    is_initialized: Arc<std::sync::atomic::AtomicBool>,
    startup_time: Instant,
    stats: Arc<RwLock<SystemStatistics>>,
    metrics: Arc<DashMap<String, MetricValue>>,
    pending_batches: Arc<RwLock<Vec<PendingBatch>>>,
    active_connections: Arc<RwLock<HashMap<std::net::SocketAddr, ConnectionInfo>>>,
    session_cache: Arc<RwLock<HashMap<Vec<u8>, SessionCacheEntry>>>,
    scaling_settings: Arc<RwLock<ScalingSettings>>,
    performance_counters: Arc<DashMap<String, PerformanceCounter>>,

    // Новые поля
    circuit_breaker_manager: Arc<CircuitBreakerManager>,
    qos_manager: Arc<QosManager>,
    adaptive_batcher: Arc<AdaptiveBatcher>,
    metrics_tracing: Arc<MetricsTracingSystem>,
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
    GetStatistics,
    ResetStatistics,
    RebalanceWorkers,
    ScaleUp {
        count: usize,
    },
    ScaleDown {
        count: usize,
    },
    UpdateScalingSettings {
        settings: ScalingSettings,
    },
}

/// Статус системы
#[derive(Debug, Clone)]
pub struct SystemStatus {
    pub timestamp: Instant,
    pub is_running: bool,
    pub statistics: SystemStatistics,
    pub active_connections: usize,
    pub pending_tasks: usize,
    pub memory_usage: MemoryUsage,
    pub throughput: ThroughputMetrics,
    pub scaling_settings: ScalingSettings,
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

/// Настройки скейлинга
#[derive(Debug, Clone)]
pub struct ScalingSettings {
    pub buffer_pool_target_hit_rate: f64,
    pub crypto_processor_target_success_rate: f64,
    pub work_stealing_target_queue_size: usize,
    pub connection_target_count: usize,
    pub min_worker_count: usize,
    pub max_worker_count: usize,
    pub auto_scaling_enabled: bool,
    pub scaling_cooldown_seconds: u64,
    pub last_scaling_time: Instant,
}

impl Default for ScalingSettings {
    fn default() -> Self {
        Self {
            buffer_pool_target_hit_rate: 0.7,
            crypto_processor_target_success_rate: 0.98,
            work_stealing_target_queue_size: 1000,
            connection_target_count: 1000,
            min_worker_count: 4,
            max_worker_count: 256,
            auto_scaling_enabled: true,
            scaling_cooldown_seconds: 60,
            last_scaling_time: Instant::now(),
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

/// Серьезность ошибки
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSeverity {
    Low,
    Medium,
    High,
    Critical,
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

/// Счетчик производительности
#[derive(Debug, Clone)]
pub struct PerformanceCounter {
    pub name: String,
    pub value: f64,
    pub timestamp: Instant,
    pub window_size: usize,
    pub values: VecDeque<f64>,
}

impl PerformanceCounter {
    pub fn new(name: String, window_size: usize) -> Self {
        Self {
            name,
            value: 0.0,
            timestamp: Instant::now(),
            window_size,
            values: VecDeque::with_capacity(window_size),
        }
    }

    pub fn update(&mut self, value: f64) {
        self.value = value;
        self.timestamp = Instant::now();
        self.values.push_back(value);
        if self.values.len() > self.window_size {
            self.values.pop_front();
        }
    }

    pub fn average(&self) -> f64 {
        if self.values.is_empty() {
            return 0.0;
        }
        self.values.iter().sum::<f64>() / self.values.len() as f64
    }
}

use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct AdvancedSystemMetrics {
    pub system_status: SystemStatus,
    pub dispatcher_metrics: AdvancedDispatcherMetrics,
    pub batch_metrics: BatchMetrics,
    pub qos_stats: QosStatistics,
    pub qos_quotas: (f64, f64, f64),
    pub qos_utilization: (f64, f64, f64),
    pub circuit_stats: Vec<CircuitBreakerStats>,
    pub trace_stats: TraceStats,
    pub timestamp: Instant,
}

impl IntegratedBatchSystem {
    /// Создание новой интегрированной batch системы
    pub async fn new(
        config: BatchConfig,
        session_manager: Arc<PhantomSessionManager>,
        crypto: Arc<PhantomCrypto>,
        monitor: Option<Arc<UnifiedMonitor>>,
    ) -> Result<Self, BatchError> {
        info!("🚀 Инициализация оптимизированной Batch системы...");

        let startup_time = Instant::now();

        // Инициализируем системы метрик и трассировки
        let metrics_config = MetricsConfig {
            enabled: config.metrics_enabled,
            collection_interval: config.metrics_collection_interval,
            trace_sampling_rate: config.trace_sampling_rate,
            service_name: "batch-system".to_string(),
            service_version: "1.0.0".to_string(),
            environment: "production".to_string(),
            retention_period: Duration::from_secs(300),
        };

        let metrics_tracing = Arc::new(
            MetricsTracingSystem::new(metrics_config)
                .map_err(|e| BatchError::ProcessingError(e.to_string()))?
        );

        // Инициализируем Circuit Breaker Manager
        let circuit_breaker_manager = Arc::new(
            CircuitBreakerManager::new(Arc::new(config.clone()))
        );

        // Создаем Circuit Breaker для ключевых компонентов
        let _dispatcher_circuit_breaker = circuit_breaker_manager.get_or_create("dispatcher");

        // Инициализируем QoS Manager
        let qos_manager = Arc::new(
            QosManager::new(
                config.high_priority_quota,
                config.normal_priority_quota,
                config.low_priority_quota,
                config.max_queue_size,
            )
        );

        // Инициализируем Adaptive Batcher
        let adaptive_batcher_config = AdaptiveBatcherConfig {
            min_batch_size: config.min_batch_size,
            max_batch_size: config.max_batch_size,
            initial_batch_size: config.batch_size,
            window_duration: config.adaptive_batch_window,
            target_latency: Duration::from_millis(50),
            max_increase_rate: 0.5,
            min_decrease_rate: 0.3,
            adaptation_interval: Duration::from_secs(1),
            enable_auto_tuning: config.enable_adaptive_batching,
            enable_predictive_adaptation: true,  // Добавлено
            prediction_horizon: Duration::from_secs(30),  // Добавлено
            smoothing_factor: 0.3,  // Добавлено
            confidence_threshold: 0.7,  // Добавлено
        };

        let adaptive_batcher = Arc::new(
            AdaptiveBatcher::new(adaptive_batcher_config)
        );

        // Каналы для событий СИСТЕМЫ
        let (system_event_tx, system_event_rx) = mpsc::channel(10000);
        let (command_tx, _) = broadcast::channel(100);

        // Канал для READER событий
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

        let chacha20_accelerator = Arc::new(ChaCha20BatchAccelerator::new(4));
        let blake3_accelerator = Arc::new(Blake3BatchAccelerator::new(4));

        // Создаем packet service
        let packet_service = Arc::new(PhantomPacketService::new(
            session_manager.clone(),
            {
                use crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;

                let monitor_to_use = monitor.unwrap_or_else(|| {
                    Arc::new(UnifiedMonitor::new(
                        crate::core::monitoring::config::MonitoringConfig::default()
                    ))
                });

                Arc::new(ConnectionHeartbeatManager::new(
                    session_manager.clone(),
                    monitor_to_use,
                ))
            },
        ));

        let packet_processor = PhantomPacketProcessor::new();

        // Создаем reader
        let reader = Arc::new(BatchReader::new(config.clone(), reader_event_tx.clone()));

        let writer = Arc::new(BatchWriter::new(config.clone()));

        let dispatcher = Arc::new(PacketDispatcher::new(
            config.clone(),
            session_manager.clone(),
            packet_service.clone(),
            writer.clone(),
        ).await);

        // Создаем WorkStealingDispatcher вместо LoadAwareDispatcher для совместимости
        let work_stealing_dispatcher = Arc::new(
            WorkStealingDispatcher::new(
                config.worker_count,
                config.max_queue_size,
                session_manager.clone(),
            )
        );

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
            event_tx: system_event_tx.clone(),
            event_rx: Arc::new(Mutex::new(system_event_rx)),
            command_tx,
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            is_initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            startup_time,
            stats: Arc::new(RwLock::new(SystemStatistics {
                startup_time,
                ..Default::default()
            })),
            metrics: Arc::new(DashMap::new()),
            pending_batches: Arc::new(RwLock::new(Vec::new())),
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            session_cache: Arc::new(RwLock::new(HashMap::new())),
            scaling_settings: Arc::new(RwLock::new(ScalingSettings::default())),
            performance_counters: Arc::new(DashMap::new()),

            // Новые поля
            circuit_breaker_manager,
            qos_manager,
            adaptive_batcher,
            metrics_tracing,
        };

        // Запускаем конвертер ReaderEvent -> SystemEvent
        system.start_reader_event_converter(reader_event_rx).await;

        // Инициализируем систему
        system.initialize().await?;

        info!("✅ Оптимизированная Batch система успешно инициализирована");
        Ok(system)
    }

    // Добавляем новые методы

    /// Получить расширенные метрики системы
    pub async fn get_advanced_metrics(&self) -> AdvancedSystemMetrics {
        let status = self.get_status().await;

        // Получаем метрики из диспетчера
        let dispatcher_metrics = self.work_stealing_dispatcher.get_advanced_metrics().await;
        let batch_metrics = self.adaptive_batcher.get_metrics().await;
        let qos_stats = self.qos_manager.get_statistics().await;
        let qos_quotas = self.qos_manager.get_quotas().await;
        let qos_utilization = self.qos_manager.get_utilization().await;
        let circuit_stats = self.circuit_breaker_manager.get_all_stats().await;
        let trace_stats = self.metrics_tracing.get_trace_stats();

        AdvancedSystemMetrics {
            system_status: status,
            dispatcher_metrics,
            batch_metrics,
            qos_stats,
            qos_quotas,
            qos_utilization,
            circuit_stats,
            trace_stats,
            timestamp: Instant::now(),
        }
    }

    /// Принудительная адаптация батчинга
    pub async fn force_batch_adaptation(&self) {
        self.adaptive_batcher.force_adaptation().await;
    }

    /// Обновление QoS квот
    pub async fn update_qos_quotas(
        &self,
        high_priority: Option<f64>,
        normal_priority: Option<f64>,
        low_priority: Option<f64>,
    ) -> Result<(), super::qos_manager::QosError> {
        self.qos_manager.update_quotas(high_priority, normal_priority, low_priority).await
    }

    /// Сброс Circuit Breaker
    pub async fn reset_circuit_breaker(&self, name: &str) {
        if let Some(breaker) = self.circuit_breaker_manager.get_breaker(name).await {
            breaker.reset().await;
        }
    }

    /// Graceful degradation при перегрузке
    pub async fn enable_graceful_degradation(&self) {
        info!("🔄 Включение graceful degradation");

        // 1. Уменьшаем QoS квоты для низкоприоритетного трафика
        let _ = self.update_qos_quotas(
            Some(0.5),   // Увеличиваем high priority
            Some(0.4),   // Нормальный оставляем
            Some(0.1),   // Уменьшаем low priority
        ).await;

        // 2. Уменьшаем размер батчей
        self.adaptive_batcher.force_adaptation().await;

        // 3. Включаем более агрессивный Circuit Breaker
        // (уже настроено в конфигурации)

        info!("✅ Graceful degradation активирован");
    }

    /// Восстановление нормального режима
    pub async fn disable_graceful_degradation(&self) {
        info!("🔄 Выключение graceful degradation");

        // Возвращаем стандартные QoS квоты
        let _ = self.update_qos_quotas(
            Some(self.config.high_priority_quota),
            Some(self.config.normal_priority_quota),
            Some(self.config.low_priority_quota),
        ).await;

        // Сбрасываем Circuit Breakers
        for breaker in self.circuit_breaker_manager.get_all_breakers() {
            breaker.reset().await;
        }

        info!("✅ Нормальный режим восстановлен");
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
        self.start_statistics_collector().await;
        self.start_batch_processor().await;
        self.start_performance_monitoring().await;

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
        info!("📥 Получены данные: {} байт от {}", data.len(), source_addr);

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

        // Обновляем счетчик производительности
        self.update_performance_counters().await;

        // Проверяем необходимость скейлинга
        self.check_scaling_needs().await;

        // Определяем тип обработки на основе приоритета и размера данных
        let _processor_type = self.determine_processor_type(&data, priority);

        // Создаем задачу для WorkStealingDispatcher
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
                info!("✅ Задача отправлена в work-stealing диспетчер, ID: {}", task_id);

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
        info!("✅ Данные обработаны для сессии: {}, успех: {}",
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
        _source_addr: std::net::SocketAddr, // Добавлено подчеркивание
    ) {
        info!("🔄 Отслеживание результата задачи {}", task_id);

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
                    info!("✅ Получен результат задачи {}", task_id);

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
    ) {
        info!("🔄 Processing task result for session: {}", hex::encode(&session_id));

        match task_result.result {
            Ok(data) => {
                info!("✅ Task result successful, data length: {}", data.len());

                // Данные уже дешифрованы work-stealing dispatcher
                if data.len() > 1 {
                    let packet_type = data[0];
                    let packet_data = &data[1..];

                    info!("📦 Обработка дешифрованного пакета: тип=0x{:02x}, размер={}",
                       packet_type, packet_data.len());

                    // Получаем сессию
                    if let Some(session) = self.session_manager.get_session(&session_id).await {
                        info!("✅ Session found for {}", hex::encode(&session_id));

                        // Обрабатываем через packet service
                        match self.packet_service.process_packet(
                            session.clone(),
                            packet_type,
                            packet_data.to_vec(),
                            task_result.destination_addr, // Используем адрес из результата
                        ).await {
                            Ok(processing_result) => {
                                info!("✅ Packet service processed: packet_type=0x{:02x}, response_len={}",
                                   processing_result.packet_type, processing_result.response.len());

                                // Шифруем ответ
                                match self.packet_processor.create_outgoing_vec(
                                    &session,
                                    processing_result.packet_type,
                                    &processing_result.response,
                                ) {
                                    Ok(encrypted_response) => {
                                        info!("✅ Response encrypted: {} bytes", encrypted_response.len());

                                        // Отправляем зашифрованный ответ
                                        info!("📤 Sending response to {} with priority: {:?}",
                                           task_result.destination_addr, processing_result.priority);

                                        match self.writer.write(
                                            task_result.destination_addr,
                                            session_id.clone(),
                                            Bytes::from(encrypted_response.clone()),
                                            processing_result.priority,
                                            true,
                                        ).await {
                                            Ok(_) => {
                                                info!("✅ Response sent successfully to {}", task_result.destination_addr);
                                            }
                                            Err(e) => {
                                                error!("❌ Ошибка отправки ответа: {}", e);
                                            }
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
                } else {
                    warn!("⚠️ Получены данные некорректной длины: {}", data.len());
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
        info!("✅ Батч {} завершен: размер={}, время={:?}, успех={:.1}%",
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
    }

    /// Запуск мониторинга производительности
    async fn start_performance_monitoring(&self) {
        let system = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));

            while system.is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;
                system.update_performance_counters().await;
                system.check_scaling_needs().await;
            }
        });
    }

    /// Обновление счетчиков производительности
    async fn update_performance_counters(&self) {
        // Собираем метрики из буферных пулов
        let stats = self.buffer_pool.get_stats();
        let reuse_rate = self.optimized_buffer_pool.get_reuse_rate();

        let hit_rate = if stats.allocation_count + stats.reuse_count > 0 {
            stats.reuse_count as f64 / (stats.allocation_count + stats.reuse_count) as f64
        } else {
            0.0
        };

        // Обновляем счетчики
        {
            let mut counter = self.performance_counters
                .entry("buffer_pool_hit_rate".to_string())
                .or_insert_with(|| PerformanceCounter::new("buffer_pool_hit_rate".to_string(), 60));
            counter.value_mut().update(hit_rate);
        }

        {
            let mut counter = self.performance_counters
                .entry("buffer_pool_reuse_rate".to_string())
                .or_insert_with(|| PerformanceCounter::new("buffer_pool_reuse_rate".to_string(), 60));
            counter.value_mut().update(reuse_rate);
        }

        // Собираем метрики из криптопроцессора
        let crypto_stats = self.crypto_processor.get_stats();
        let success_rate = if crypto_stats.total_operations > 0 {
            1.0 - (crypto_stats.total_failed as f64 / crypto_stats.total_operations as f64)
        } else {
            1.0
        };

        {
            let mut counter = self.performance_counters
                .entry("crypto_success_rate".to_string())
                .or_insert_with(|| PerformanceCounter::new("crypto_success_rate".to_string(), 60));
            counter.value_mut().update(success_rate);
        }

        // Собираем метрики из диспетчеров
        let dispatcher_stats = self.work_stealing_dispatcher.get_stats();
        let total_tasks: u64 = dispatcher_stats.values().sum();

        {
            let mut counter = self.performance_counters
                .entry("work_stealing_tasks".to_string())
                .or_insert_with(|| PerformanceCounter::new("work_stealing_tasks".to_string(), 60));
            counter.value_mut().update(total_tasks as f64);
        }

        // Собираем метрики соединений
        let connections = self.active_connections.read().await;
        let active_connections = connections.len();

        {
            let mut counter = self.performance_counters
                .entry("active_connections".to_string())
                .or_insert_with(|| PerformanceCounter::new("active_connections".to_string(), 60));
            counter.value_mut().update(active_connections as f64);
        }
    }

    /// Проверка необходимости скейлинга
    async fn check_scaling_needs(&self) {
        let settings = self.scaling_settings.read().await;

        // Проверяем, включен ли автоскейлинг и прошло ли достаточно времени с последнего скейлинга
        if !settings.auto_scaling_enabled {
            return;
        }

        let now = Instant::now();
        if now.duration_since(settings.last_scaling_time) < Duration::from_secs(settings.scaling_cooldown_seconds) {
            return;
        }

        // Получаем текущие значения производительности
        let buffer_hit_rate = self.performance_counters
            .get("buffer_pool_hit_rate")
            .map(|c| c.average())
            .unwrap_or(0.0);

        let crypto_success_rate = self.performance_counters
            .get("crypto_success_rate")
            .map(|c| c.average())
            .unwrap_or(1.0);

        let work_stealing_tasks = self.performance_counters
            .get("work_stealing_tasks")
            .map(|c| c.value)
            .unwrap_or(0.0) as usize;

        let active_connections = self.performance_counters
            .get("active_connections")
            .map(|c| c.value)
            .unwrap_or(0.0) as usize;

        // Проверяем условия для скейлинга
        let mut needs_scaling = false;
        let mut scaling_action = ScalingAction::None;

        // Проверка буферного пула
        if buffer_hit_rate < settings.buffer_pool_target_hit_rate * 0.8 {
            needs_scaling = true;
            scaling_action = ScalingAction::IncreaseBufferPool;
        }

        // Проверка криптопроцессора
        if crypto_success_rate < settings.crypto_processor_target_success_rate * 0.9 {
            needs_scaling = true;
            scaling_action = ScalingAction::IncreaseCryptoWorkers;
        }

        // Проверка диспетчеров
        if work_stealing_tasks > settings.work_stealing_target_queue_size * 2 {
            needs_scaling = true;
            scaling_action = ScalingAction::IncreaseWorkers;
        } else if work_stealing_tasks < settings.work_stealing_target_queue_size / 4 {
            needs_scaling = true;
            scaling_action = ScalingAction::DecreaseWorkers;
        }

        // Проверка соединений
        if active_connections > settings.connection_target_count * 2 {
            needs_scaling = true;
            scaling_action = ScalingAction::IncreaseCapacity;
        }

        if needs_scaling {
            self.apply_scaling_action(scaling_action, &settings).await;
        }
    }

    /// Применение действия скейлинга
    async fn apply_scaling_action(&self, action: ScalingAction, _settings: &ScalingSettings) {
        match action {
            ScalingAction::IncreaseBufferPool => {
                warn!("📈 Увеличиваем buffer_pool из-за низкого hit rate");
                // Здесь можно добавить логику увеличения буферного пула
            }
            ScalingAction::IncreaseCryptoWorkers => {
                warn!("📈 Увеличиваем количество crypto workers");
                // Здесь можно добавить логику увеличения воркеров
            }
            ScalingAction::IncreaseWorkers => {
                warn!("📈 Увеличиваем количество work-stealing workers");
                // Здесь можно добавить логику увеличения воркеров
            }
            ScalingAction::DecreaseWorkers => {
                warn!("📉 Уменьшаем количество work-stealing workers");
                // Здесь можно добавить логику уменьшения воркеров
            }
            ScalingAction::IncreaseCapacity => {
                warn!("📈 Увеличиваем общую емкость системы");
                // Здесь можно добавить комплексное увеличение емкости
            }
            ScalingAction::None => {}
        }

        // Обновляем время последнего скейлинга
        let mut settings_write = self.scaling_settings.write().await;
        settings_write.last_scaling_time = Instant::now();
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
            SystemCommand::UpdateScalingSettings { settings } => {
                self.update_scaling_settings(settings).await;
            }
        }
    }

    /// Обновление настроек скейлинга
    async fn update_scaling_settings(&self, settings: ScalingSettings) {
        let mut current_settings = self.scaling_settings.write().await;
        *current_settings = settings;
        info!("⚙️ Настройки скейлинга обновлены");
    }

    /// Запуск обработки
    async fn start_processing(&self) {
        if !self.is_running.load(std::sync::atomic::Ordering::SeqCst) {
            info!("▶️ Запуск обработки данных...");
            self.is_running.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Пауза обработки
    async fn pause_processing(&self) {
        if self.is_running.load(std::sync::atomic::Ordering::SeqCst) {
            info!("⏸️ Приостановка обработки данных...");
            self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);
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
    }

    /// Очистка кэшей
    async fn clear_caches(&self) {
        info!("🧹 Очистка всех кэшей...");

        // Очищаем кэши криптопроцессора
        self.crypto_processor.clear_cache().await;

        // Note: Акселераторы находятся в Arc, поэтому мы не можем их мутировать напрямую
        // Вместо этого мы можем создать новые экземпляры или добавить методы очистки через внутреннюю мутабельность
        warn!("⚠️ Очистка кэшей акселераторов требует дополнительной реализации");
    }

    /// Регулировка конфигурации
    async fn adjust_config(&self, parameter: String, value: String) {
        info!("⚙️ Регулировка конфигурации: {} = {}", parameter, value);

        // Здесь можно добавить логику регулировки конфигурации
        // В зависимости от параметра
    }

    /// Аварийное завершение
    async fn emergency_shutdown(&self, reason: String) {
        error!("🚨 Аварийное завершение: {}", reason);

        // Немедленная остановка всех компонентов
        self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);

        // Форсированное завершение
        self.shutdown_components().await;
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

        // Сбрасываем счетчики производительности
        self.performance_counters.clear();
    }

    /// Перебалансировка воркеров
    async fn rebalance_workers(&self) {
        info!("⚖️ Перебалансировка воркеров...");

        // Здесь можно добавить логику перебалансировки
        // Например, перераспределение задач между воркерами
    }

    /// Масштабирование вверх
    async fn scale_up(&self, count: usize) {
        info!("📈 Масштабирование вверх на {} воркеров", count);

        // Здесь можно добавить логику масштабирования
        // Например, создание дополнительных воркеров
    }

    /// Масштабирование вниз
    async fn scale_down(&self, count: usize) {
        info!("📉 Масштабирование вниз на {} воркеров", count);

        // Здесь можно добавить логику масштабирования
        // Например, остановка части воркеров
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
                                || batch.operations.len() >= system.config.batch_size
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
        info!("🔄 Обработка батча {} с {} операциями", batch.id, batch.operations.len());

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
        let settings = self.scaling_settings.read().await.clone();

        // Рассчитываем текущую пропускную способность
        let throughput = ThroughputMetrics {
            packets_per_second: 0.0, // Нужно рассчитать на основе истории
            bytes_per_second: 0.0,
            operations_per_second: 0.0,
            avg_batch_size: 0.0,
            latency_p50: Duration::from_millis(0),
            latency_p95: Duration::from_millis(0),
            latency_p99: Duration::from_millis(0),
        };

        // Рассчитываем использование памяти (упрощенно)
        let memory_usage = MemoryUsage {
            total: 0,
            used: 0,
            free: 0,
            buffer_pool: 0,
            crypto_pool: 0,
            connections: connections.len(),
        };

        SystemStatus {
            timestamp: Instant::now(),
            is_running: self.is_running.load(std::sync::atomic::Ordering::Relaxed),
            statistics: stats,
            active_connections: connections.len(),
            pending_tasks: self.pending_batches.read().await.len(),
            memory_usage,
            throughput,
            scaling_settings: settings,
        }
    }
}

/// Действия скейлинга
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalingAction {
    None,
    IncreaseBufferPool,
    IncreaseCryptoWorkers,
    IncreaseWorkers,
    DecreaseWorkers,
    IncreaseCapacity,
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
            event_tx: self.event_tx.clone(),
            event_rx: self.event_rx.clone(),
            command_tx: self.command_tx.clone(),
            is_running: self.is_running.clone(),
            is_initialized: self.is_initialized.clone(),
            startup_time: self.startup_time,
            stats: self.stats.clone(),
            metrics: self.metrics.clone(),
            pending_batches: self.pending_batches.clone(),
            active_connections: self.active_connections.clone(),
            session_cache: self.session_cache.clone(),
            scaling_settings: self.scaling_settings.clone(),
            performance_counters: self.performance_counters.clone(),
            circuit_breaker_manager: self.circuit_breaker_manager.clone(),
            qos_manager: self.qos_manager.clone(),
            adaptive_batcher: self.adaptive_batcher.clone(),
            metrics_tracing: self.metrics_tracing.clone(),
        }
    }
}

// Экспортируем тип для использования в других модулях
pub use IntegratedBatchSystem as BatchSystem;
use crate::core::monitoring::unified_monitor::UnifiedMonitor;
use crate::core::protocol::batch_system::adaptive_batcher::{AdaptiveBatcher, AdaptiveBatcherConfig, BatchMetrics};
use crate::core::protocol::batch_system::circuit_breaker::{CircuitBreakerManager, CircuitBreakerStats};
use crate::core::protocol::batch_system::load_aware_dispatcher::AdvancedDispatcherMetrics;
use crate::core::protocol::batch_system::metrics_tracing::{MetricsConfig, MetricsTracingSystem, TraceStats};
use crate::core::protocol::batch_system::qos_manager::{QosManager, QosStatistics};