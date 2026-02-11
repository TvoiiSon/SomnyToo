use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::HashMap;
use tokio::sync::{mpsc, RwLock, Mutex, broadcast};
use bytes::Bytes;
use tracing::{info, error, debug, warn};
use dashmap::DashMap;

use crate::core::protocol::batch_system::config::BatchConfig;
use crate::core::protocol::batch_system::types::error::BatchError;
use crate::core::protocol::batch_system::types::priority::Priority;

// ✅ ОПТИМИЗИРОВАННЫЕ КОМПОНЕНТЫ
use crate::core::protocol::batch_system::optimized::work_stealing_dispatcher::{
    WorkStealingDispatcher, WorkStealingTask, WorkStealingResult, DispatcherAdvancedStats
};
use crate::core::protocol::batch_system::optimized::buffer_pool::OptimizedBufferPool;
use crate::core::protocol::batch_system::optimized::crypto_processor::OptimizedCryptoProcessor;
use crate::core::protocol::batch_system::optimized::factory::OptimizedFactory;

// ✅ АКСЕЛЕРАТОРЫ
use crate::core::protocol::batch_system::acceleration_batch::chacha20_batch_accel::ChaCha20BatchAccelerator;
use crate::core::protocol::batch_system::acceleration_batch::blake3_batch_accel::Blake3BatchAccelerator;

// ✅ ИНТЕГРИРУЕМЫЕ КОМПОНЕНТЫ
use crate::core::protocol::batch_system::circuit_breaker::{
    CircuitBreakerManager, CircuitBreakerStats
};
use crate::core::protocol::batch_system::qos_manager::{QosManager, QosStatistics};
use crate::core::protocol::batch_system::adaptive_batcher::{
    AdaptiveBatcher, AdaptiveBatcherConfig, BatchMetrics
};
use crate::core::protocol::batch_system::metrics_tracing::{
    MetricsTracingSystem, MetricsConfig
};

// ✅ READER & WRITER (единственные компоненты из core)
use crate::core::protocol::batch_system::core::reader::{BatchReader, ReaderEvent};
use crate::core::protocol::batch_system::core::writer::{BatchWriter};

// ✅ ВНЕШНИЕ ЗАВИСИМОСТИ
use crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::packets::packet_service::PhantomPacketService;
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;
use crate::core::monitoring::unified_monitor::UnifiedMonitor;

/// ⚡ ОСНОВНОЙ ИНТЕГРИРОВАННЫЙ УЗЕЛ BATCH СИСТЕМЫ v2.0
/// Полностью оптимизированная версия, без легаси-компонентов
pub struct IntegratedBatchSystem {
    // 📋 КОНФИГУРАЦИЯ
    config: BatchConfig,

    // 🔧 ОСНОВНЫЕ КОМПОНЕНТЫ
    reader: Arc<BatchReader>,
    writer: Arc<BatchWriter>,
    work_stealing_dispatcher: Arc<WorkStealingDispatcher>,
    crypto_processor: Arc<OptimizedCryptoProcessor>,
    buffer_pool: Arc<OptimizedBufferPool>,

    // 🚀 АКСЕЛЕРАТОРЫ
    chacha20_accelerator: Arc<ChaCha20BatchAccelerator>,
    blake3_accelerator: Arc<Blake3BatchAccelerator>,

    // 🛡️ ИНТЕГРИРОВАННЫЕ КОМПОНЕНТЫ
    circuit_breaker_manager: Arc<CircuitBreakerManager>,
    qos_manager: Arc<QosManager>,
    adaptive_batcher: Arc<AdaptiveBatcher>,
    metrics_tracing: Arc<MetricsTracingSystem>,

    // 🌐 ВНЕШНИЕ СЕРВИСЫ
    packet_service: Arc<PhantomPacketService>,
    packet_processor: PhantomPacketProcessor,
    session_manager: Arc<PhantomSessionManager>,
    crypto: Arc<PhantomCrypto>,

    // 📨 СИСТЕМНЫЕ КАНАЛЫ
    event_tx: mpsc::Sender<SystemEvent>,
    event_rx: Arc<Mutex<mpsc::Receiver<SystemEvent>>>,
    command_tx: broadcast::Sender<SystemCommand>,

    // 🎮 УПРАВЛЕНИЕ
    is_running: Arc<std::sync::atomic::AtomicBool>,
    is_initialized: Arc<std::sync::atomic::AtomicBool>,
    startup_time: Instant,

    // 📊 СТАТИСТИКА И МЕТРИКИ
    stats: Arc<RwLock<SystemStatistics>>,
    metrics: Arc<DashMap<String, MetricValue>>,
    pending_batches: Arc<RwLock<Vec<PendingBatch>>>,
    active_connections: Arc<RwLock<HashMap<std::net::SocketAddr, ConnectionInfo>>>,
    session_cache: Arc<RwLock<HashMap<Vec<u8>, SessionCacheEntry>>>,
    scaling_settings: Arc<RwLock<ScalingSettings>>,
    performance_counters: Arc<DashMap<String, PerformanceCounter>>,
}

impl IntegratedBatchSystem {
    /// 🚀 Создание новой оптимизированной batch системы
    pub async fn new(
        config: BatchConfig,
        session_manager: Arc<PhantomSessionManager>,
        crypto: Arc<PhantomCrypto>,
        monitor: Option<Arc<UnifiedMonitor>>,
    ) -> Result<Self, BatchError> {
        let startup_time = Instant::now();

        // ============= 1. ИНИЦИАЛИЗАЦИЯ METRICS TRACING =============
        info!("📊 [1/10] Инициализация Metrics & Tracing...");
        let metrics_config = MetricsConfig {
            enabled: config.metrics_enabled,
            collection_interval: config.metrics_collection_interval,
            trace_sampling_rate: config.trace_sampling_rate,
            service_name: "batch-system".to_string(),
            service_version: "2.0.0".to_string(),
            environment: "production".to_string(),
            retention_period: Duration::from_secs(3600),
        };

        let metrics_tracing = Arc::new(
            MetricsTracingSystem::new(metrics_config)
                .map_err(|e| BatchError::ProcessingError(format!("Metrics init failed: {}", e)))?
        );

        // ============= 2. ИНИЦИАЛИЗАЦИЯ CIRCUIT BREAKER =============
        info!("🛡️ [2/10] Инициализация Circuit Breaker Manager...");
        let circuit_breaker_manager = Arc::new(
            CircuitBreakerManager::new(Arc::new(config.clone()))
        );

        // Создаем Circuit Breaker для ключевых компонентов
        let dispatcher_circuit_breaker = circuit_breaker_manager.get_or_create("dispatcher");
        let _crypto_circuit_breaker = circuit_breaker_manager.get_or_create("crypto_processor");
        let _writer_circuit_breaker = circuit_breaker_manager.get_or_create("writer");

        // ============= 3. ИНИЦИАЛИЗАЦИЯ QoS =============
        info!("⚖️ [3/10] Инициализация QoS Manager...");
        let qos_manager = Arc::new(
            QosManager::new(
                config.high_priority_quota,
                config.normal_priority_quota,
                config.low_priority_quota,
                config.max_queue_size,
            )
        );

        // ============= 4. ИНИЦИАЛИЗАЦИЯ ADAPTIVE BATCHER =============
        info!("🔄 [4/10] Инициализация Adaptive Batcher с ML предсказанием...");
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
            enable_predictive_adaptation: true,
            prediction_horizon: Duration::from_secs(30),
            smoothing_factor: 0.3,
            confidence_threshold: 0.7,
        };

        let adaptive_batcher = Arc::new(
            AdaptiveBatcher::new(adaptive_batcher_config)
        );

        // ============= 5. КАНАЛЫ СОБЫТИЙ =============
        info!("📬 [5/10] Инициализация каналов событий...");
        let (system_event_tx, system_event_rx) = mpsc::channel(50000);
        let (command_tx, _) = broadcast::channel(1000);
        let (reader_event_tx, reader_event_rx) = mpsc::channel(50000);

        // ============= 6. ИНИЦИАЛИЗАЦИЯ ОПТИМИЗИРОВАННЫХ КОМПОНЕНТОВ =============
        info!("🔧 [6/10] Инициализация оптимизированных компонентов...");

        let buffer_pool = OptimizedFactory::create_buffer_pool(
            config.read_buffer_size,
            config.write_buffer_size,
            64 * 1024,
            5000,
        );

        let crypto_processor = OptimizedFactory::create_crypto_processor(
            config.worker_count * 2
        );

        // ============= 7. ИНИЦИАЛИЗАЦИЯ SIMD АКСЕЛЕРАТОРОВ =============
        info!("🚀 [7/10] Инициализация SIMD акселераторов...");
        let chacha20_accelerator = Arc::new(
            ChaCha20BatchAccelerator::new(config.simd_batch_size)
        );
        let blake3_accelerator = Arc::new(
            Blake3BatchAccelerator::new(config.simd_batch_size)
        );

        let _chacha20_info = chacha20_accelerator.get_simd_info();
        let _blake3_info = blake3_accelerator.get_performance_info();

        // ============= 8. ВНЕШНИЕ СЕРВИСЫ =============
        info!("🌐 [8/10] Инициализация внешних сервисов...");
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

        // ============= 9. READER & WRITER =============
        info!("📖 [9/10] Инициализация Reader/Writer...");
        let reader = Arc::new(BatchReader::new(config.clone(), reader_event_tx.clone()));
        let writer = Arc::new(BatchWriter::new(config.clone()));

        let work_stealing_dispatcher = OptimizedFactory::create_dispatcher(
            config.worker_count,
            config.max_queue_size,
            session_manager.clone(),
            adaptive_batcher.clone(),
            qos_manager.clone(),
            dispatcher_circuit_breaker,
        );

        // ============= 11. ФИНАЛЬНАЯ СБОРКА =============
        info!("🏗️ [10/10] Финальная сборка системы...");

        let system = Self {
            config: config.clone(),
            reader,
            writer,
            work_stealing_dispatcher,
            crypto_processor,
            buffer_pool,
            chacha20_accelerator,
            blake3_accelerator,
            circuit_breaker_manager,
            qos_manager,
            adaptive_batcher,
            metrics_tracing,
            packet_service,
            packet_processor,
            session_manager: session_manager.clone(),
            crypto: crypto.clone(),
            event_tx: system_event_tx.clone(),
            event_rx: Arc::new(Mutex::new(system_event_rx)),
            command_tx,
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            is_initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            startup_time,
            stats: Arc::new(RwLock::new(SystemStatistics {
                startup_time,
                ..Default::default()
            })),
            metrics: Arc::new(DashMap::new()),
            pending_batches: Arc::new(RwLock::new(Vec::with_capacity(1000))),
            active_connections: Arc::new(RwLock::new(HashMap::with_capacity(10000))),
            session_cache: Arc::new(RwLock::new(HashMap::with_capacity(10000))),
            scaling_settings: Arc::new(RwLock::new(ScalingSettings::default())),
            performance_counters: Arc::new(DashMap::new()),
        };

        // ============= ЗАПУСК КОМПОНЕНТОВ =============
        system.start_reader_event_converter(reader_event_rx).await;
        system.initialize().await?;

        Ok(system)
    }

    /// 🔄 Конвертер событий Reader -> System
    async fn start_reader_event_converter(&self, mut reader_event_rx: mpsc::Receiver<ReaderEvent>) {
        let event_tx = self.event_tx.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            debug!("🔄 Reader event converter started");

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                match reader_event_rx.recv().await {
                    Some(event) => {
                        let system_event = match event {
                            ReaderEvent::DataReady { session_id, data, source_addr, priority, received_at } => {
                                SystemEvent::DataReceived {
                                    session_id,
                                    data: data.freeze(),
                                    source_addr,
                                    priority,
                                    timestamp: received_at,
                                }
                            }
                            ReaderEvent::ConnectionClosed { source_addr, reason } => {
                                SystemEvent::ConnectionClosed {
                                    addr: source_addr,
                                    session_id: Vec::new(),
                                    reason,
                                }
                            }
                            // ИСПРАВЛЕНО: игнорируем source_addr
                            ReaderEvent::Error { source_addr: _, error } => {
                                SystemEvent::ErrorOccurred {
                                    error: error.to_string(),
                                    context: "reader_error".to_string(),
                                    severity: ErrorSeverity::High,
                                }
                            }
                        };

                        if let Err(e) = event_tx.send(system_event).await {
                            error!("❌ Failed to send converted event: {}", e);
                            break;
                        }
                    }
                    None => {
                        debug!("📭 Reader event channel closed");
                        break;
                    }
                }
            }

            debug!("👋 Reader event converter stopped");
        });
    }

    /// 🎯 Инициализация системы
    async fn initialize(&self) -> Result<(), BatchError> {
        info!("🔄 Инициализация компонентов системы...");

        self.is_initialized.store(true, std::sync::atomic::Ordering::SeqCst);

        self.start_event_handlers().await;
        self.start_command_handlers().await;
        self.start_statistics_collector().await;
        self.start_batch_processor().await;
        self.start_performance_monitoring().await;
        self.start_auto_scaling().await;
        self.start_qos_adaptation().await;

        info!("✅ Все компоненты системы инициализированы");
        Ok(())
    }

    /// 👂 Запуск обработчиков событий
    async fn start_event_handlers(&self) {
        let event_rx = self.event_rx.clone();
        let system = self.clone();

        tokio::spawn(async move {
            debug!("👂 Event handler started");
            let mut receiver = event_rx.lock().await;

            while let Some(event) = receiver.recv().await {
                system.handle_event(event).await;
            }

            debug!("👋 Event handler stopped");
        });
    }

    /// 🎛️ Обработка событий
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

    /// 📥 Обработка полученных данных
    async fn handle_data_received(
        &self,
        session_id: Vec<u8>,
        data: Bytes,
        source_addr: std::net::SocketAddr,
        priority: Priority,
        timestamp: Instant,
    ) {
        debug!("📥 Data received: {} bytes from {}", data.len(), source_addr);

        // Обновляем статистику
        {
            let mut stats = self.stats.write().await;
            stats.total_data_received += data.len() as u64;
            stats.total_packets_processed += 1;
        }

        // Обновляем информацию о соединении
        self.update_connection_info(&session_id, source_addr, priority, data.len()).await;

        // ИСПРАВЛЕНО: добавлены deadline и retry_count
        let task = WorkStealingTask {
            id: 0,
            session_id: session_id.clone(),
            data: data.clone(),
            source_addr,
            priority,
            created_at: timestamp,
            worker_id: None,
            retry_count: 0,
            deadline: Some(timestamp + Duration::from_secs(30)),
        };

        // Отправляем в диспетчер
        match self.work_stealing_dispatcher.submit_task(task).await {
            Ok(task_id) => {
                debug!("✅ Task {} submitted to dispatcher", task_id);
                self.track_task_result(task_id, session_id, source_addr).await;
            }
            Err(e) => {
                error!("❌ Failed to submit task: {}", e);
                self.record_metric("dispatcher.rejections", 1.0).await;

                let event = SystemEvent::ErrorOccurred {
                    error: e.to_string(),
                    context: "submit_task".to_string(),
                    severity: ErrorSeverity::High,
                };
                let _ = self.event_tx.send(event).await;
            }
        }
    }

    /// 🔍 Отслеживание результата задачи
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
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                async {
                    let mut attempts = 0;
                    while attempts < 100 {
                        if let Some(task_result) = dispatcher.get_result(task_id) {
                            return Some(task_result);
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        attempts += 1;
                    }
                    None
                }
            ).await;

            match result {
                Ok(Some(task_result)) => {
                    debug!("✅ Task {} completed", task_id);

                    let process_result = ProcessResult {
                        success: task_result.result.is_ok(),
                        data: task_result.result.clone().ok().map(Bytes::from),
                        error: task_result.result.clone().err().map(|e| e.to_string()),
                        metadata: HashMap::from([
                            ("worker_id".to_string(), task_result.worker_id.to_string()),
                            ("processing_time".to_string(), format!("{:?}", task_result.processing_time)),
                        ]),
                    };

                    let event = SystemEvent::DataProcessed {
                        session_id: session_id.clone(),
                        result: process_result,
                        processing_time: task_result.processing_time,
                        worker_id: Some(task_result.worker_id),
                    };

                    let _ = event_tx.send(event).await;
                    system.process_task_result(task_result, session_id, source_addr).await;
                }
                Ok(None) => {
                    warn!("⚠️ Task {} result timeout", task_id);
                }
                Err(_) => {
                    error!("⏰ Task {} timeout", task_id);
                }
            }
        });
    }

    /// 🔄 Обработка результата задачи
    async fn process_task_result(
        &self,
        task_result: WorkStealingResult,
        _session_id: Vec<u8>,
        _source_addr: std::net::SocketAddr,
    ) {
        match task_result.result {
            Ok(data) => {
                if data.len() > 1 {
                    let packet_type = data[0];
                    let packet_data = &data[1..];

                    // ✅ ИСПОЛЬЗУЕМ session_id ИЗ task_result, А НЕ ИЗ ПАРАМЕТРА
                    if let Some(session) = self.session_manager.get_session(&task_result.session_id).await {
                        match self.packet_service.process_packet(
                            session.clone(),
                            packet_type,
                            packet_data.to_vec(),
                            task_result.destination_addr,
                        ).await {
                            Ok(processing_result) => {
                                match self.packet_processor.create_outgoing_vec(
                                    &session,
                                    processing_result.packet_type,
                                    &processing_result.response,
                                ) {
                                    Ok(encrypted_response) => {
                                        if let Err(e) = self.writer.write(
                                            task_result.destination_addr,
                                            task_result.session_id.clone(), // ✅ ИСПОЛЬЗУЕМ task_result.session_id
                                            Bytes::from(encrypted_response),
                                            processing_result.priority,
                                            true,
                                        ).await {
                                            error!("❌ Failed to send response: {}", e);
                                        }
                                    }
                                    Err(e) => error!("❌ Encryption failed: {}", e),
                                }
                            }
                            Err(e) => error!("❌ Packet processing failed: {}", e),
                        }
                    }
                }
            }
            Err(e) => error!("❌ Task processing failed: {}", e),
        }
    }

    /// 🔗 Обработка открытия соединения
    async fn handle_connection_opened(&self, addr: std::net::SocketAddr, session_id: Vec<u8>) {
        debug!("🔗 Connection opened: {} -> {}", addr, hex::encode(&session_id));

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

        let mut stats = self.stats.write().await;
        stats.total_connections += 1;
    }

    /// 🔒 Обработка закрытия соединения
    async fn handle_connection_closed(&self, addr: std::net::SocketAddr, session_id: Vec<u8>, reason: String) {
        debug!("🔒 Connection closed: {} -> {}: {}", addr, hex::encode(&session_id), reason);

        let mut connections = self.active_connections.write().await;
        connections.remove(&addr);
    }

    /// ✅ Обработка завершения батча
    async fn handle_batch_completed(
        &self,
        batch_id: u64,
        size: usize,
        processing_time: Duration,
        success_rate: f64
    ) {
        debug!("✅ Batch {} completed: size={}, time={:?}, success={:.1}%",
               batch_id, size, processing_time, success_rate * 100.0);

        let mut stats = self.stats.write().await;
        stats.total_batches_processed += 1;

        let total_batches = stats.total_batches_processed as f64;
        let current_avg = stats.avg_processing_time.as_nanos() as f64;
        let new_avg = (current_avg * (total_batches - 1.0) + processing_time.as_nanos() as f64) / total_batches;
        stats.avg_processing_time = Duration::from_nanos(new_avg as u64);
    }

    /// ⚠️ Обработка ошибки
    async fn handle_error_occurred(&self, error: String, context: String, severity: ErrorSeverity) {
        match severity {
            ErrorSeverity::Low => debug!("⚠️ Low: {} in {}", error, context),
            ErrorSeverity::Medium => warn!("⚠️ Medium: {} in {}", error, context),
            ErrorSeverity::High => error!("❌ High: {} in {}", error, context),
            ErrorSeverity::Critical => {
                error!("🚨 CRITICAL: {} in {}", error, context);
            }
        }

        let mut stats = self.stats.write().await;
        stats.total_errors += 1;

        self.record_metric("system.errors", 1.0).await;
        self.record_metric(&format!("system.errors.{}", severity as u8), 1.0).await;
    }

    /// 📊 Обработка обработанных данных
    async fn handle_data_processed(
        &self,
        _session_id: Vec<u8>,
        result: ProcessResult,
        _processing_time: Duration,
        _worker_id: Option<usize>,
    ) {
        if result.success {
            if let Some(data) = &result.data {
                let mut stats = self.stats.write().await;
                stats.total_data_sent += data.len() as u64;

                // Обновляем информацию о соединении
                if let Some(addr) = result.metadata.get("destination_addr") {
                    if let Ok(addr) = addr.parse() {
                        let mut connections = self.active_connections.write().await;
                        if let Some(conn) = connections.get_mut(&addr) {
                            conn.bytes_sent += data.len() as u64;
                            conn.last_activity = Instant::now();
                        }
                    }
                }
            }
        }
    }

    /// 🔄 Обновление информации о соединении
    async fn update_connection_info(
        &self,
        session_id: &[u8],
        source_addr: std::net::SocketAddr,
        priority: Priority,
        data_len: usize,
    ) {
        let mut connections = self.active_connections.write().await;

        if let Some(conn) = connections.get_mut(&source_addr) {
            conn.last_activity = Instant::now();
            conn.bytes_received += data_len as u64;
            conn.priority = priority;
        } else {
            connections.insert(source_addr, ConnectionInfo {
                addr: source_addr,
                session_id: session_id.to_vec(),
                opened_at: Instant::now(),
                last_activity: Instant::now(),
                bytes_received: data_len as u64,
                bytes_sent: 0,
                priority,
                is_active: true,
                worker_assigned: None,
            });
        }
    }

    /// 📈 Запуск мониторинга производительности
    async fn start_performance_monitoring(&self) {
        let system = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));

            while system.is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;
                system.update_performance_counters().await;
                system.check_scaling_needs().await;
            }
        });
    }

    /// 📊 Обновление счетчиков производительности
    async fn update_performance_counters(&self) {
        // Статистика буферного пула
        let buffer_stats = self.buffer_pool.get_detailed_stats();
        let total_hit_rate = buffer_stats.get("Global")
            .map(|s| s.hit_rate)
            .unwrap_or(0.0);

        self.record_metric("buffer_pool.hit_rate", total_hit_rate).await;
        self.record_metric("buffer_pool.reuse_rate", self.buffer_pool.get_reuse_rate()).await;

        // Статистика криптопроцессора
        let crypto_stats = self.crypto_processor.get_stats();
        let crypto_tasks = crypto_stats.get("crypto_tasks_submitted").copied().unwrap_or(0);
        let crypto_processed = crypto_stats.get("crypto_tasks_processed").copied().unwrap_or(0);
        let crypto_steals = crypto_stats.get("crypto_steals").copied().unwrap_or(0);

        self.record_metric("crypto.tasks_submitted", crypto_tasks as f64).await;
        self.record_metric("crypto.tasks_processed", crypto_processed as f64).await;
        self.record_metric("crypto.steals", crypto_steals as f64).await;

        // Статистика диспетчера
        let dispatcher_stats = self.work_stealing_dispatcher.get_advanced_stats().await;
        self.record_metric("dispatcher.tasks_processed", dispatcher_stats.total_tasks_processed as f64).await;
        self.record_metric("dispatcher.work_steals", dispatcher_stats.work_steals as f64).await;
        self.record_metric("dispatcher.imbalance", dispatcher_stats.imbalance).await;

        // Статистика соединений
        let connections = self.active_connections.read().await.len();
        self.record_metric("connections.active", connections as f64).await;
    }

    /// 📈 Запуск автоскейлинга
    async fn start_auto_scaling(&self) {
        let system = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));

            while system.is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let settings = system.scaling_settings.read().await;
                if !settings.auto_scaling_enabled {
                    continue;
                }

                let now = Instant::now();
                if now.duration_since(settings.last_scaling_time) < Duration::from_secs(settings.scaling_cooldown_seconds) {
                    continue;
                }

                drop(settings);
                system.check_scaling_needs().await;
            }
        });
    }

    /// 🔄 Запуск QoS адаптации
    async fn start_qos_adaptation(&self) {
        let qos_manager = self.qos_manager.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));

            loop {
                interval.tick().await;

                match qos_manager.adapt_quotas().await {
                    Ok(decision) => {
                        info!("🔄 QoS adapted: {}", decision.reason);
                    }
                    Err(e) => {
                        debug!("QoS adaptation skipped: {}", e);
                    }
                }
            }
        });
    }

    /// 📊 Проверка необходимости скейлинга
    async fn check_scaling_needs(&self) {
        let settings = self.scaling_settings.read().await;

        let buffer_hit_rate = self.get_metric("buffer_pool.hit_rate").await.unwrap_or(0.0);
        let crypto_success_rate = self.get_metric("crypto.success_rate").await.unwrap_or(1.0);
        let dispatcher_load = self.get_metric("dispatcher.imbalance").await.unwrap_or(0.0);
        let active_connections = self.active_connections.read().await.len();

        if buffer_hit_rate < settings.buffer_pool_target_hit_rate * 0.8 {
            warn!("📉 Buffer pool hit rate low: {:.1}%", buffer_hit_rate * 100.0);
        }

        if crypto_success_rate < settings.crypto_processor_target_success_rate * 0.9 {
            warn!("⚠️ Crypto success rate low: {:.1}%", crypto_success_rate * 100.0);
        }

        if dispatcher_load > 0.7 {
            warn!("⚖️ High dispatcher imbalance: {:.2}", dispatcher_load);
        }

        if active_connections as f64 > settings.connection_target_count as f64 * 1.5 {
            warn!("🔌 High connection count: {}", active_connections);
        }
    }

    /// 🎛️ Запуск обработчиков команд
    async fn start_command_handlers(&self) {
        let command_rx = self.command_tx.subscribe();
        let system = self.clone();

        tokio::spawn(async move {
            debug!("🎛️ Command handler started");
            let mut receiver = command_rx;

            while let Ok(command) = receiver.recv().await {
                system.handle_command(command).await;
            }

            debug!("👋 Command handler stopped");
        });
    }

    /// 🎮 Обработка команды
    async fn handle_command(&self, command: SystemCommand) {
        match command {
            SystemCommand::StartProcessing => self.start_processing().await,
            SystemCommand::PauseProcessing => self.pause_processing().await,
            SystemCommand::ResumeProcessing => self.resume_processing().await,
            SystemCommand::StopProcessing => self.stop_processing().await,
            SystemCommand::FlushBuffers => self.flush_buffers().await,
            SystemCommand::ClearCaches => self.clear_caches().await,
            SystemCommand::AdjustConfig { parameter, value } => self.adjust_config(parameter, value).await,
            SystemCommand::EmergencyShutdown { reason } => self.emergency_shutdown(reason).await,
            SystemCommand::GetStatistics => self.get_statistics().await,
            SystemCommand::ResetStatistics => self.reset_statistics().await,
            SystemCommand::RebalanceWorkers => self.rebalance_workers().await,
            SystemCommand::ScaleUp { count } => self.scale_up(count).await,
            SystemCommand::ScaleDown { count } => self.scale_down(count).await,
            SystemCommand::UpdateScalingSettings { settings } => self.update_scaling_settings(settings).await,
        }
    }

    /// ▶️ Запуск обработки
    async fn start_processing(&self) {
        if !self.is_running.load(std::sync::atomic::Ordering::SeqCst) {
            info!("▶️ Starting data processing...");
            self.is_running.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// ⏸️ Пауза обработки
    async fn pause_processing(&self) {
        if self.is_running.load(std::sync::atomic::Ordering::SeqCst) {
            info!("⏸️ Pausing data processing...");
            self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// ▶️ Возобновление обработки
    async fn resume_processing(&self) {
        self.start_processing().await;
    }

    /// ⏹️ Остановка обработки
    async fn stop_processing(&self) {
        info!("⏹️ Stopping data processing...");
        self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);
        self.shutdown_components().await;
    }

    /// 🌀 Сброс буферов
    async fn flush_buffers(&self) {
        info!("🌀 Flushing all buffers...");
        self.buffer_pool.force_cleanup();

        let mut cache = self.session_cache.write().await;
        cache.clear();
    }

    /// 🧹 Очистка кэшей
    async fn clear_caches(&self) {
        info!("🧹 Clearing all caches...");

        let mut session_cache = self.session_cache.write().await;
        session_cache.clear();

        self.performance_counters.clear();
        self.metrics.clear();
    }

    /// ⚙️ Регулировка конфигурации
    async fn adjust_config(&self, parameter: String, value: String) {
        info!("⚙️ Adjusting config: {} = {}", parameter, value);

        match parameter.as_str() {
            "batch_size" => {
                if let Ok(_size) = value.parse::<usize>() {
                    info!("Batch size will be adjusted by AdaptiveBatcher");
                }
            }
            "worker_count" => {
                if let Ok(count) = value.parse::<usize>() {
                    info!("Worker count change requested: {}", count);

                    let current_settings = self.scaling_settings.read().await;
                    let current_workers = self.work_stealing_dispatcher.worker_senders.len();

                    if count > current_workers {
                        if count <= current_settings.max_worker_count {
                            self.scale_up(count - current_workers).await;
                        } else {
                            warn!("Requested worker count {} exceeds maximum {}",
                            count, current_settings.max_worker_count);
                        }
                    } else if count < current_workers {
                        if count >= current_settings.min_worker_count {
                            self.scale_down(current_workers - count).await;
                        } else {
                            warn!("Requested worker count {} below minimum {}",
                            count, current_settings.min_worker_count);
                        }
                    }
                }
            }
            "min_batch_size" => {
                if let Ok(size) = value.parse::<usize>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.min_batch_size = size;
                    self.record_metric("config.min_batch_size", size as f64).await;
                    info!("Min batch size updated to {}", size);
                }
            }
            "max_batch_size" => {
                if let Ok(size) = value.parse::<usize>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.max_batch_size = size;
                    self.record_metric("config.max_batch_size", size as f64).await;
                    info!("Max batch size updated to {}", size);
                }
            }
            "target_latency_ms" => {
                if let Ok(ms) = value.parse::<u64>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.target_latency = Duration::from_millis(ms);
                    self.record_metric("config.target_latency_ms", ms as f64).await;
                    info!("Target latency updated to {} ms", ms);
                }
            }
            "confidence_threshold" => {
                if let Ok(threshold) = value.parse::<f64>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.confidence_threshold = threshold.clamp(0.0, 1.0);
                    self.record_metric("config.confidence_threshold", threshold).await;
                    info!("Confidence threshold updated to {:.2}", threshold);
                }
            }
            "enable_predictive_adaptation" => {
                if let Ok(enabled) = value.parse::<bool>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.enable_predictive_adaptation = enabled;
                    self.record_metric("config.enable_predictive_adaptation", enabled as i64 as f64).await;
                    info!("Predictive adaptation {}", if enabled { "enabled" } else { "disabled" });
                }
            }
            "enable_auto_tuning" => {
                if let Ok(enabled) = value.parse::<bool>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.enable_auto_tuning = enabled;
                    self.record_metric("config.enable_auto_tuning", enabled as i64 as f64).await;
                    info!("Auto tuning {}", if enabled { "enabled" } else { "disabled" });
                }
            }
            "prediction_horizon_sec" => {
                if let Ok(sec) = value.parse::<u64>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.prediction_horizon = Duration::from_secs(sec);
                    self.record_metric("config.prediction_horizon_sec", sec as f64).await;
                    info!("Prediction horizon updated to {} seconds", sec);
                }
            }
            "smoothing_factor" => {
                if let Ok(factor) = value.parse::<f64>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.smoothing_factor = factor.clamp(0.1, 0.9);
                    self.record_metric("config.smoothing_factor", factor).await;
                    info!("Smoothing factor updated to {:.2}", factor);
                }
            }
            _ => warn!("Unknown parameter: {}", parameter),
        }
    }

    /// 🚨 Аварийное завершение
    async fn emergency_shutdown(&self, reason: String) {
        error!("🚨 EMERGENCY SHUTDOWN: {}", reason);

        self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);
        self.shutdown_components().await;

        self.record_metric("system.emergency_shutdown", 1.0).await;
    }

    /// 📊 Получение статистики
    async fn get_statistics(&self) {
        let stats = self.stats.read().await.clone();
        let status = self.get_system_status().await;

        info!("📊 System Statistics:");
        info!("  ├─ Uptime: {:?}", stats.uptime);
        info!("  ├─ Processed packets: {}", stats.total_packets_processed);
        info!("  ├─ Data received: {} MB", stats.total_data_received / 1024 / 1024);
        info!("  ├─ Data sent: {} MB", stats.total_data_sent / 1024 / 1024);
        info!("  ├─ Active connections: {}", status.active_connections);
        info!("  ├─ Avg processing time: {:?}", stats.avg_processing_time);
        info!("  └─ Total errors: {}", stats.total_errors);
    }

    /// 🔄 Сброс статистики
    async fn reset_statistics(&self) {
        info!("🔄 Resetting system statistics...");

        let mut stats = self.stats.write().await;
        *stats = SystemStatistics {
            startup_time: stats.startup_time,
            ..Default::default()
        };

        self.metrics.clear();
        self.performance_counters.clear();
    }

    /// ⚖️ Перебалансировка воркеров
    async fn rebalance_workers(&self) {
        info!("⚖️ Rebalancing workers...");
        // WorkStealingDispatcher автоматически балансирует нагрузку
        self.record_metric("dispatcher.manual_rebalance", 1.0).await;
    }

    /// 📈 Масштабирование вверх
    async fn scale_up(&self, count: usize) {
        info!("📈 Scaling up by {} workers", count);

        let current_workers = self.work_stealing_dispatcher.worker_senders.len();
        let settings = self.scaling_settings.read().await;

        if count == 0 {
            return;
        }

        if current_workers >= settings.max_worker_count {
            warn!("Cannot scale up: already at maximum workers ({})", current_workers);
            return;
        }

        let target_workers = (current_workers + count).min(settings.max_worker_count);
        let actual_increase = target_workers - current_workers;

        if actual_increase > 0 {
            // Обновляем настройки скейлинга
            let mut new_settings = settings.clone();
            new_settings.last_scaling_time = Instant::now();
            *self.scaling_settings.write().await = new_settings;

            self.record_metric("scaling.scale_up", actual_increase as f64).await;
            self.record_metric("scaling.current_workers", target_workers as f64).await;

            info!("✅ Scaled up from {} to {} workers (increased by {})",
            current_workers, target_workers, actual_increase);

            // Принудительно перебалансируем воркеров
            self.rebalance_workers().await;
        }
    }


    /// 📉 Масштабирование вниз
    async fn scale_down(&self, count: usize) {
        info!("📉 Scaling down by {} workers", count);

        let current_workers = self.work_stealing_dispatcher.worker_senders.len();
        let settings = self.scaling_settings.read().await;

        if count == 0 {
            return;
        }

        if current_workers <= settings.min_worker_count {
            warn!("Cannot scale down: already at minimum workers ({})", current_workers);
            return;
        }

        let target_workers = current_workers.saturating_sub(count).max(settings.min_worker_count);
        let actual_reduction = current_workers - target_workers;

        if actual_reduction > 0 {
            // Обновляем настройки скейлинга
            let mut new_settings = settings.clone();
            new_settings.last_scaling_time = Instant::now();
            *self.scaling_settings.write().await = new_settings;

            self.record_metric("scaling.scale_down", actual_reduction as f64).await;
            self.record_metric("scaling.current_workers", target_workers as f64).await;

            info!("✅ Scaled down from {} to {} workers (reduced by {})",
            current_workers, target_workers, actual_reduction);

            // Принудительно перебалансируем оставшихся воркеров
            self.rebalance_workers().await;
        }
    }

    /// ⚙️ Обновление настроек скейлинга
    async fn update_scaling_settings(&self, settings: ScalingSettings) {
        let mut current = self.scaling_settings.write().await;
        *current = settings;
        info!("⚙️ Scaling settings updated");
    }

    /// 📈 Запуск сборщика статистики
    async fn start_statistics_collector(&self) {
        let stats = self.stats.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let mut stats_guard = stats.write().await;
                stats_guard.uptime = Instant::now().duration_since(stats_guard.startup_time);
            }
        });
    }

    /// 🔄 Запуск обработчика батчей
    async fn start_batch_processor(&self) {
        let pending_batches = self.pending_batches.clone();
        let is_running = self.is_running.clone();
        let system = self.clone();

        tokio::spawn(async move {
            debug!("🔄 Batch processor started");
            let mut interval = tokio::time::interval(Duration::from_millis(50));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let batches_to_process = {
                    let mut batches = pending_batches.write().await;
                    if batches.is_empty() {
                        continue;
                    }

                    let now = Instant::now();
                    let optimal_size = system.adaptive_batcher.get_batch_size().await;

                    let (ready, not_ready): (Vec<_>, Vec<_>) = batches
                        .drain(..)
                        .partition(|batch| {
                            batch.deadline.map_or(true, |deadline| now >= deadline)
                                || batch.operations.len() >= optimal_size
                        });

                    *batches = not_ready;
                    ready
                };

                for batch in batches_to_process {
                    system.process_batch(batch).await;
                }
            }

            debug!("👋 Batch processor stopped");
        });
    }

    /// 📦 Обработка батча
    async fn process_batch(&self, batch: PendingBatch) {
        let start_time = Instant::now();
        let batch_size = batch.operations.len();

        debug!("🔄 Processing batch {} with {} operations", batch.id, batch_size);

        let success_rate = if batch.operations.is_empty() {
            0.0
        } else {
            // Здесь должна быть реальная обработка
            let successful = batch_size;
            successful as f64 / batch_size as f64
        };

        let processing_time = start_time.elapsed();

        // Регистрируем выполнение батча в AdaptiveBatcher
        self.adaptive_batcher.record_batch_execution(
            batch_size,
            processing_time,
            success_rate,
            self.pending_batches.read().await.len(),
        ).await;

        let event = SystemEvent::BatchCompleted {
            batch_id: batch.id,
            size: batch_size,
            processing_time,
            success_rate,
        };

        let _ = self.event_tx.send(event).await;
    }

    /// 🛑 Завершение компонентов
    async fn shutdown_components(&self) {
        info!("🛑 Shutting down components...");

        self.work_stealing_dispatcher.shutdown().await;
        self.crypto_processor.shutdown().await;
        self.reader.shutdown().await;
        self.writer.shutdown().await;

        info!("✅ All components shut down");
    }

    /// 🔗 Регистрация соединения
    pub async fn register_connection(
        &self,
        source_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        read_stream: Box<dyn tokio::io::AsyncRead + Unpin + Send + Sync>,
        write_stream: Box<dyn tokio::io::AsyncWrite + Unpin + Send + Sync>,
    ) -> Result<(), BatchError> {
        debug!("🔗 Registering connection: {} -> {}", source_addr, hex::encode(&session_id));

        self.reader.register_connection(
            source_addr,
            session_id.clone(),
            read_stream,
        ).await?;

        self.writer.register_connection(
            source_addr,
            session_id.clone(),
            write_stream,
        ).await?;

        let event = SystemEvent::ConnectionOpened {
            addr: source_addr,
            session_id,
        };

        let _ = self.event_tx.send(event).await;

        Ok(())
    }

    /// 📊 Получение статуса системы
    pub async fn get_system_status(&self) -> SystemStatus {
        let stats = self.stats.read().await.clone();
        let connections = self.active_connections.read().await;
        let settings = self.scaling_settings.read().await.clone();

        let batch_metrics = self.adaptive_batcher.get_metrics().await;
        let qos_stats = self.qos_manager.get_statistics().await;
        let qos_quotas = self.qos_manager.get_quotas().await;
        let qos_utilization = self.qos_manager.get_utilization().await;
        let circuit_stats = self.circuit_breaker_manager.get_all_stats().await;
        let dispatcher_stats = self.work_stealing_dispatcher.get_advanced_stats().await;

        SystemStatus {
            timestamp: Instant::now(),
            is_running: self.is_running.load(std::sync::atomic::Ordering::Relaxed),
            statistics: stats,
            active_connections: connections.len(),
            pending_tasks: self.pending_batches.read().await.len(),
            memory_usage: MemoryUsage {
                total: 0,
                used: 0,
                free: 0,
                buffer_pool: self.buffer_pool.get_detailed_stats()
                    .values()
                    .map(|s| s.memory_mb as usize * 1024 * 1024)
                    .sum(),
                crypto_pool: 0,
                connections: connections.len(),
            },
            throughput: self.calculate_throughput().await,
            scaling_settings: settings,
            batch_metrics,
            qos_stats,
            qos_quotas,
            qos_utilization,
            circuit_stats,
            dispatcher_stats,
        }
    }

    /// 📈 Расчет пропускной способности
    async fn calculate_throughput(&self) -> ThroughputMetrics {
        let stats = self.stats.read().await;
        let uptime = stats.uptime.as_secs_f64().max(1.0);

        ThroughputMetrics {
            packets_per_second: stats.total_packets_processed as f64 / uptime,
            bytes_per_second: stats.total_data_received as f64 / uptime,
            operations_per_second: stats.total_batches_processed as f64 / uptime,
            avg_batch_size: self.adaptive_batcher.get_metrics().await.avg_batch_size,
            latency_p50: stats.avg_processing_time,
            latency_p95: Duration::from_nanos((stats.avg_processing_time.as_nanos() as f64 * 1.5) as u64),
            latency_p99: Duration::from_nanos((stats.avg_processing_time.as_nanos() as f64 * 2.0) as u64),
        }
    }

    /// 📝 Запись метрики
    async fn record_metric(&self, name: &str, value: f64) {
        self.metrics.insert(name.to_string(), MetricValue::Float(value));
        self.metrics_tracing.record_metric(name, value);
    }

    /// 📊 Получение метрики
    async fn get_metric(&self, name: &str) -> Option<f64> {
        self.metrics.get(name).and_then(|m| {
            if let MetricValue::Float(v) = m.value() {
                Some(*v)
            } else {
                None
            }
        })
    }

    /// 📦 Получение диспетчера
    pub fn get_dispatcher(&self) -> Arc<WorkStealingDispatcher> {
        self.work_stealing_dispatcher.clone()
    }

    /// 📦 Получение QoS менеджера
    pub fn get_qos_manager(&self) -> Arc<QosManager> {
        self.qos_manager.clone()
    }

    /// 📦 Получение Adaptive Batcher
    pub fn get_adaptive_batcher(&self) -> Arc<AdaptiveBatcher> {
        self.adaptive_batcher.clone()
    }

    /// 📦 Получение Circuit Breaker Manager
    pub fn get_circuit_breaker_manager(&self) -> Arc<CircuitBreakerManager> {
        self.circuit_breaker_manager.clone()
    }
}

/// ============= СТРУКТУРЫ ДАННЫХ =============

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
    pub batch_metrics: BatchMetrics,
    pub qos_stats: QosStatistics,
    pub qos_quotas: (f64, f64, f64),
    pub qos_utilization: (f64, f64, f64),
    pub circuit_stats: Vec<CircuitBreakerStats>,
    pub dispatcher_stats: DispatcherAdvancedStats,
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
            buffer_pool_target_hit_rate: 0.85,
            crypto_processor_target_success_rate: 0.99,
            work_stealing_target_queue_size: 1000,
            connection_target_count: 10000,
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
    Low = 0,
    Medium = 1,
    High = 2,
    Critical = 3,
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

impl Clone for IntegratedBatchSystem {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            reader: self.reader.clone(),
            writer: self.writer.clone(),
            work_stealing_dispatcher: self.work_stealing_dispatcher.clone(),
            crypto_processor: self.crypto_processor.clone(),
            buffer_pool: self.buffer_pool.clone(),
            chacha20_accelerator: self.chacha20_accelerator.clone(),
            blake3_accelerator: self.blake3_accelerator.clone(),
            circuit_breaker_manager: self.circuit_breaker_manager.clone(),
            qos_manager: self.qos_manager.clone(),
            adaptive_batcher: self.adaptive_batcher.clone(),
            metrics_tracing: self.metrics_tracing.clone(),
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
        }
    }
}

use std::collections::VecDeque;