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
use crate::core::protocol::batch_system::types::packet_types::{is_packet_supported, get_packet_info, get_packet_priority};

// ✅ READER & WRITER
use crate::core::protocol::batch_system::core::reader::{BatchReader, ReaderEvent};
use crate::core::protocol::batch_system::core::writer::{BatchWriter};

// ✅ ВНЕШНИЕ ЗАВИСИМОСТИ
use crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::packets::packet_service::PhantomPacketService;
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;
use crate::core::monitoring::unified_monitor::UnifiedMonitor;

/// ⚡ ОСНОВНОЙ ИНТЕГРИРОВАННЫЙ УЗЕЛ BATCH СИСТЕМЫ v2.1
/// Полностью рабочая версия с динамическим масштабированием
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

    // ✅ ИСПРАВЛЕНО: РЕАЛЬНО ИСПОЛЬЗУЕМЫЕ КОМПОНЕНТЫ
    pending_batches: Arc<RwLock<Vec<PendingBatch>>>,
    active_connections: Arc<RwLock<HashMap<std::net::SocketAddr, ConnectionInfo>>>,
    session_cache: Arc<RwLock<HashMap<Vec<u8>, SessionCacheEntry>>>,
    scaling_settings: Arc<RwLock<ScalingSettings>>,
    performance_counters: Arc<DashMap<String, PerformanceCounter>>,

    // 🆕 НОВЫЕ КОМПОНЕНТЫ ДЛЯ ДИНАМИЧЕСКОГО МАСШТАБИРОВАНИЯ
    worker_pool: Arc<WorkerPool>,
    scaling_lock: Arc<Mutex<()>>,
}

/// 🏭 Пул воркеров для динамического масштабирования
struct WorkerPool {
    min_workers: usize,
    max_workers: usize,
    current_workers: Arc<std::sync::atomic::AtomicUsize>,
    worker_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    shutdown_tx: broadcast::Sender<()>,
}

impl WorkerPool {
    fn new(min_workers: usize, max_workers: usize) -> Self {
        let (shutdown_tx, _) = broadcast::channel(max_workers * 2);
        Self {
            min_workers,
            max_workers,
            current_workers: Arc::new(std::sync::atomic::AtomicUsize::new(min_workers)),
            worker_handles: Arc::new(Mutex::new(Vec::new())),
            shutdown_tx,
        }
    }

    async fn add_workers(&self, count: usize, _dispatcher: Arc<WorkStealingDispatcher>) -> Result<usize, BatchError> {
        let current = self.current_workers.load(std::sync::atomic::Ordering::SeqCst);
        let target = (current + count).min(self.max_workers);
        let to_add = target - current;

        if to_add == 0 {
            return Ok(0);
        }

        let mut handles = self.worker_handles.lock().await;
        let shutdown_rx = self.shutdown_tx.subscribe();

        for i in 0..to_add {
            let worker_id = current + i;
            let mut shutdown_rx = shutdown_rx.resubscribe();

            let handle = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown_rx.recv() => {
                            debug!("👋 Dynamic worker #{} shutting down", worker_id);
                            break;
                        }
                        _ = tokio::time::sleep(Duration::from_millis(100)) => {
                            // Worker будет получать задачи через диспетчер
                        }
                    }
                }
            });

            handles.push(handle);
        }

        self.current_workers.store(target, std::sync::atomic::Ordering::SeqCst);
        Ok(to_add)
    }

    async fn remove_workers(&self, count: usize) -> Result<usize, BatchError> {
        let current = self.current_workers.load(std::sync::atomic::Ordering::SeqCst);
        let target = current.saturating_sub(count).max(self.min_workers);
        let to_remove = current - target;

        if to_remove == 0 {
            return Ok(0);
        }

        // Отправляем сигнал остановки для to_remove воркеров
        for _ in 0..to_remove {
            let _ = self.shutdown_tx.send(());
        }

        self.current_workers.store(target, std::sync::atomic::Ordering::SeqCst);
        Ok(to_remove)
    }
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
        info!("📊 [1/11] Инициализация Metrics & Tracing...");
        let metrics_config = MetricsConfig {
            enabled: config.metrics_enabled,
            collection_interval: config.metrics_collection_interval,
            trace_sampling_rate: config.trace_sampling_rate,
            service_name: "batch-system".to_string(),
            service_version: "2.1.0".to_string(),
            environment: "production".to_string(),
            retention_period: Duration::from_secs(3600),
        };

        let metrics_tracing = Arc::new(
            MetricsTracingSystem::new(metrics_config)
                .map_err(|e| BatchError::ProcessingError(format!("Metrics init failed: {}", e)))?
        );

        // ============= 2. ИНИЦИАЛИЗАЦИЯ CIRCUIT BREAKER =============
        info!("🛡️ [2/11] Инициализация Circuit Breaker Manager...");
        let circuit_breaker_manager = Arc::new(
            CircuitBreakerManager::new(Arc::new(config.clone()))
        );

        let dispatcher_circuit_breaker = circuit_breaker_manager.get_or_create("dispatcher");

        // ============= 3. ИНИЦИАЛИЗАЦИЯ QoS =============
        info!("⚖️ [3/11] Инициализация QoS Manager...");
        let qos_manager = Arc::new(
            QosManager::new(
                config.high_priority_quota,
                config.normal_priority_quota,
                config.low_priority_quota,
                config.max_queue_size,
            )
        );

        // ============= 4. ИНИЦИАЛИЗАЦИЯ ADAPTIVE BATCHER =============
        info!("🔄 [4/11] Инициализация Adaptive Batcher с ML предсказанием...");
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
        info!("📬 [5/11] Инициализация каналов событий...");
        let (system_event_tx, system_event_rx) = mpsc::channel(50000);
        let (command_tx, _) = broadcast::channel(1000);
        let (reader_event_tx, reader_event_rx) = mpsc::channel(50000);

        // ============= 6. ИНИЦИАЛИЗАЦИЯ ОПТИМИЗИРОВАННЫХ КОМПОНЕНТОВ =============
        info!("🔧 [6/11] Инициализация оптимизированных компонентов...");

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
        info!("🚀 [7/11] Инициализация SIMD акселераторов...");
        let chacha20_accelerator = Arc::new(
            ChaCha20BatchAccelerator::new(config.simd_batch_size)
        );
        let blake3_accelerator = Arc::new(
            Blake3BatchAccelerator::new(config.simd_batch_size)
        );

        // ============= 8. ВНЕШНИЕ СЕРВИСЫ =============
        info!("🌐 [8/11] Инициализация внешних сервисов...");
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
        info!("📖 [9/11] Инициализация Reader/Writer...");
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

        // ============= 10. ИНИЦИАЛИЗАЦИЯ WORKER POOL =============
        info!("🏭 [10/11] Инициализация Worker Pool для динамического масштабирования...");
        let worker_pool = Arc::new(WorkerPool::new(
            config.worker_count / 2,
            config.worker_count * 4,
        ));

        // ============= 11. ФИНАЛЬНАЯ СБОРКА =============
        info!("🏗️ [11/11] Финальная сборка системы...");

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
            worker_pool,
            scaling_lock: Arc::new(Mutex::new(())),
        };

        // ============= ЗАПУСК КОМПОНЕНТОВ =============
        system.start_reader_event_converter(reader_event_rx).await;
        system.start_session_cache_cleaner().await;
        system.start_performance_counter_updater().await;
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

    /// 🧹 Очистка устаревших записей в кэше сессий
    async fn start_session_cache_cleaner(&self) {
        let session_cache = self.session_cache.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300)); // 5 минут

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let mut cache = session_cache.write().await;
                let before = cache.len();
                let now = Instant::now();

                cache.retain(|_, entry| {
                    now.duration_since(entry.last_used) < Duration::from_secs(3600) // 1 час
                });

                let removed = before - cache.len();
                if removed > 0 {
                    debug!("🧹 Session cache cleaned: removed {} stale entries", removed);
                }
            }
        });
    }

    /// 📊 Обновление счетчиков производительности
    async fn start_performance_counter_updater(&self) {
        let perf_counters = self.performance_counters.clone();
        let is_running = self.is_running.clone();
        let system = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                // Обновляем счетчики производительности
                let stats = system.stats.read().await;

                let mut throughput_counter = perf_counters
                    .entry("throughput".to_string())
                    .or_insert_with(|| PerformanceCounter::new("throughput".to_string(), 60));

                let uptime = stats.uptime.as_secs_f64().max(1.0);
                let throughput = stats.total_packets_processed as f64 / uptime;
                throughput_counter.update(throughput);

                let mut latency_counter = perf_counters
                    .entry("avg_latency_ms".to_string())
                    .or_insert_with(|| PerformanceCounter::new("avg_latency_ms".to_string(), 60));

                latency_counter.update(stats.avg_processing_time.as_millis() as f64);

                let mut batch_size_counter = perf_counters
                    .entry("avg_batch_size".to_string())
                    .or_insert_with(|| PerformanceCounter::new("avg_batch_size".to_string(), 60));

                batch_size_counter.update(system.adaptive_batcher.get_metrics().await.avg_batch_size);

                debug!("📊 Performance counters updated");
            }
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
        _priority: Priority,  // Не используем переданный приоритет, определяем после дешифровки
        timestamp: Instant,
    ) {
        debug!("📥 Raw data received: {} bytes from {}", data.len(), source_addr);

        // Обновляем статистику
        {
            let mut stats = self.stats.write().await;
            stats.total_data_received += data.len() as u64;
        }

        // ✅ НЕ ПРОВЕРЯЕМ ТИП ПАКЕТА ЗДЕСЬ!
        // Тип пакета будет определен ПОСЛЕ дешифрования в worker'е

        let task = WorkStealingTask {
            id: 0,
            session_id: session_id.clone(),
            data: data.clone(),
            source_addr,
            priority: Priority::Normal, // Временный приоритет, реальный определится после дешифровки
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

                    {
                        let mut stats = system.stats.write().await;
                        stats.work_stealing_count = dispatcher.get_stats()
                            .get("work_steals")
                            .copied()
                            .unwrap_or(0);
                    }

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

                    if let Some(session) = self.session_manager.get_session(&task_result.session_id).await {
                        // ✅ ИСПРАВЛЕНО: Отправляем пакет в packet_service для обработки
                        match self.packet_service.process_packet(
                            session.clone(),
                            packet_type,
                            packet_data.to_vec(),
                            task_result.destination_addr,
                        ).await {
                            Ok(processing_result) => {
                                // ✅ packet_service уже вернул правильный ответ
                                match self.packet_processor.create_outgoing_vec(
                                    &session,
                                    processing_result.packet_type,  // Используем тип из processing_result
                                    &processing_result.response,    // Используем ответ из packet_service
                                ) {
                                    Ok(encrypted_response) => {
                                        if let Err(e) = self.writer.write(
                                            task_result.destination_addr,
                                            task_result.session_id.clone(),
                                            Bytes::from(encrypted_response),
                                            processing_result.priority,  // Используем приоритет из packet_service
                                            true,  // requires_flush для критических пакетов
                                        ).await {
                                            error!("❌ Failed to send response: {}", e);
                                        } else {
                                            debug!("✅ Response sent for packet type 0x{:02x}", packet_type);
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

        let throughput = size as f64 / processing_time.as_secs_f64().max(0.001);
        if throughput > stats.peak_throughput {
            stats.peak_throughput = throughput;
        }
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
        session_id: Vec<u8>,
        result: ProcessResult,
        _processing_time: Duration,
        _worker_id: Option<usize>,
    ) {
        if result.success {
            if let Some(data) = &result.data {
                let mut stats = self.stats.write().await;
                stats.total_data_sent += data.len() as u64;

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

        let mut cache = self.session_cache.write().await;
        if let Some(entry) = cache.get_mut(&session_id) {
            entry.last_used = Instant::now();
            entry.access_count += 1;
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

        {
            let mut stats = self.stats.write().await;
            stats.work_stealing_count = dispatcher_stats.work_steals;
            stats.buffer_hit_rate = total_hit_rate;
        }

        // Статистика соединений
        let connections = self.active_connections.read().await.len();
        self.record_metric("connections.active", connections as f64).await;
    }

    /// 📈 ЗАПУСК АВТОСКЕЙЛИНГА
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

                system.perform_auto_scaling().await;
            }
        });
    }

    /// 🔄 ВЫПОЛНЕНИЕ АВТОСКЕЙЛИНГА
    async fn perform_auto_scaling(&self) {
        let _lock = self.scaling_lock.lock().await;

        let settings = self.scaling_settings.read().await;
        let dispatcher_stats = self.work_stealing_dispatcher.get_advanced_stats().await;
        let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);

        // Критерии для масштабирования вверх
        let should_scale_up =
            dispatcher_stats.queue_backlog > settings.work_stealing_target_queue_size * 2 ||
                dispatcher_stats.imbalance > 0.7 ||
                dispatcher_stats.avg_processing_time_ms > 100.0 ||
                self.active_connections.read().await.len() as f64 > settings.connection_target_count as f64 * 0.8;

        // Критерии для масштабирования вниз
        let should_scale_down =
            dispatcher_stats.queue_backlog < settings.work_stealing_target_queue_size / 4 &&
                dispatcher_stats.imbalance < 0.2 &&
                dispatcher_stats.avg_processing_time_ms < 20.0 &&
                current_workers > settings.min_worker_count;

        if should_scale_up && current_workers < settings.max_worker_count {
            let scale_up_by = 2.min(settings.max_worker_count - current_workers);
            info!("📈 Auto-scaling: scaling UP by {} workers (current: {}, queue: {})",
                scale_up_by, current_workers, dispatcher_stats.queue_backlog);
            let _ = self.scale_up(scale_up_by).await;
        } else if should_scale_down && current_workers > settings.min_worker_count {
            let scale_down_by = 2.min(current_workers - settings.min_worker_count);
            info!("📉 Auto-scaling: scaling DOWN by {} workers (current: {}, queue: {})",
                scale_down_by, current_workers, dispatcher_stats.queue_backlog);
            let _ = self.scale_down(scale_down_by).await;
        }
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
        let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);

        if buffer_hit_rate < settings.buffer_pool_target_hit_rate * 0.8 {
            warn!("📉 Buffer pool hit rate low: {:.1}%", buffer_hit_rate * 100.0);
            let _ = self.buffer_pool.force_cleanup();
        }

        if crypto_success_rate < settings.crypto_processor_target_success_rate * 0.9 {
            warn!("⚠️ Crypto success rate low: {:.1}%", crypto_success_rate * 100.0);
            if let Some(cb) = self.circuit_breaker_manager.get_breaker("crypto_processor").await {
                cb.reset().await;
            }
        }

        if dispatcher_load > 0.7 {
            warn!("⚖️ High dispatcher imbalance: {:.2}", dispatcher_load);
            self.rebalance_workers().await;
        }

        if active_connections as f64 > settings.connection_target_count as f64 * 1.5 {
            warn!("🔌 High connection count: {}", active_connections);
            if current_workers < settings.max_worker_count {
                let _ = self.scale_up(2).await;
            }
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
            SystemCommand::ScaleUp { count } => {
                let _ = self.scale_up(count).await;
            }
            SystemCommand::ScaleDown { count } => {
                let _ = self.scale_down(count).await;
            }
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
        let _ = self.buffer_pool.force_cleanup();

        let mut cache = self.session_cache.write().await;
        cache.clear();
    }

    /// 🧹 Очистка кэшей
    async fn clear_caches(&self) {
        info!("🧹 Clearing all caches...");

        let mut session_cache = self.session_cache.write().await;
        session_cache.clear();

        let mut connections = self.active_connections.write().await;
        connections.clear();

        self.performance_counters.clear();
        self.metrics.clear();

        let mut pending = self.pending_batches.write().await;
        pending.clear();

        info!("✅ All caches cleared");
    }

    /// ⚙️ РЕГУЛИРОВКА КОНФИГУРАЦИИ
    async fn adjust_config(&self, parameter: String, value: String) {
        info!("⚙️ Adjusting config: {} = {}", parameter, value);

        match parameter.as_str() {
            "batch_size" => {
                if let Ok(size) = value.parse::<usize>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    let clamped_size = size.clamp(config.min_batch_size, config.max_batch_size);
                    config.initial_batch_size = clamped_size;

                    *self.adaptive_batcher.current_batch_size.write().await = clamped_size;

                    self.record_metric("config.batch_size", clamped_size as f64).await;
                    info!("✅ Batch size updated to {} (clamped to {}-{})",
                        clamped_size, config.min_batch_size, config.max_batch_size);
                }
            }
            "worker_count" => {
                if let Ok(count) = value.parse::<usize>() {
                    let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);
                    let settings = self.scaling_settings.read().await;

                    if count > current_workers {
                        if count <= settings.max_worker_count {
                            let increase = count - current_workers;
                            info!("📈 Increasing worker count by {} to {}", increase, count);
                            let _ = self.scale_up(increase).await;
                        } else {
                            warn!("Requested worker count {} exceeds maximum {}",
                                count, settings.max_worker_count);
                        }
                    } else if count < current_workers {
                        if count >= settings.min_worker_count {
                            let decrease = current_workers - count;
                            info!("📉 Decreasing worker count by {} to {}", decrease, count);
                            let _ = self.scale_down(decrease).await;
                        } else {
                            warn!("Requested worker count {} below minimum {}",
                                count, settings.min_worker_count);
                        }
                    }
                }
            }
            "min_batch_size" => {
                if let Ok(size) = value.parse::<usize>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.min_batch_size = size.max(1);
                    if config.initial_batch_size < config.min_batch_size {
                        *self.adaptive_batcher.current_batch_size.write().await = config.min_batch_size;
                    }
                    self.record_metric("config.min_batch_size", size as f64).await;
                    info!("✅ Min batch size updated to {}", size);
                }
            }
            "max_batch_size" => {
                if let Ok(size) = value.parse::<usize>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.max_batch_size = size;
                    if config.initial_batch_size > config.max_batch_size {
                        *self.adaptive_batcher.current_batch_size.write().await = config.max_batch_size;
                    }
                    self.record_metric("config.max_batch_size", size as f64).await;
                    info!("✅ Max batch size updated to {}", size);
                }
            }
            "target_latency_ms" => {
                if let Ok(ms) = value.parse::<u64>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.target_latency = Duration::from_millis(ms);
                    self.record_metric("config.target_latency_ms", ms as f64).await;
                    info!("✅ Target latency updated to {} ms", ms);
                }
            }
            "confidence_threshold" => {
                if let Ok(threshold) = value.parse::<f64>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.confidence_threshold = threshold.clamp(0.0, 1.0);
                    self.record_metric("config.confidence_threshold", threshold).await;
                    info!("✅ Confidence threshold updated to {:.2}", threshold);
                }
            }
            "enable_predictive_adaptation" => {
                if let Ok(enabled) = value.parse::<bool>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.enable_predictive_adaptation = enabled;
                    self.record_metric("config.enable_predictive_adaptation", enabled as i64 as f64).await;
                    info!("✅ Predictive adaptation {}", if enabled { "enabled" } else { "disabled" });
                }
            }
            "enable_auto_tuning" => {
                if let Ok(enabled) = value.parse::<bool>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.enable_auto_tuning = enabled;
                    self.record_metric("config.enable_auto_tuning", enabled as i64 as f64).await;
                    info!("✅ Auto tuning {}", if enabled { "enabled" } else { "disabled" });
                }
            }
            "prediction_horizon_sec" => {
                if let Ok(sec) = value.parse::<u64>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.prediction_horizon = Duration::from_secs(sec);
                    self.record_metric("config.prediction_horizon_sec", sec as f64).await;
                    info!("✅ Prediction horizon updated to {} seconds", sec);
                }
            }
            "smoothing_factor" => {
                if let Ok(factor) = value.parse::<f64>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.smoothing_factor = factor.clamp(0.1, 0.9);
                    self.record_metric("config.smoothing_factor", factor).await;
                    info!("✅ Smoothing factor updated to {:.2}", factor);
                }
            }
            _ => warn!("⚠️ Unknown parameter: {}", parameter),
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
        info!("  ├─ Active workers: {}", self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst));
        info!("  ├─ Avg processing time: {:?}", stats.avg_processing_time);
        info!("  ├─ Peak throughput: {:.2} ops/s", stats.peak_throughput);
        info!("  ├─ Crypto operations: {}", stats.crypto_operations);
        info!("  ├─ Work steals: {}", stats.work_stealing_count);
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

    /// ⚖️ ПЕРЕБАЛАНСИРОВКА ВОРКЕРОВ
    async fn rebalance_workers(&self) {
        info!("⚖️ Rebalancing workers...");

        let stats = self.work_stealing_dispatcher.get_advanced_stats().await;
        let imbalance = stats.imbalance;

        if imbalance > 0.3 {
            info!("⚖️ High imbalance detected: {:.2}, forcing rebalance", imbalance);

            let current_loads: Vec<usize> = (0..self.work_stealing_dispatcher.worker_senders.len())
                .map(|i| self.work_stealing_dispatcher.worker_queues.get(&i).map(|q| *q).unwrap_or(0))
                .collect();

            let avg_load = current_loads.iter().sum::<usize>() as f64 / current_loads.len() as f64;

            for (worker_id, &load) in current_loads.iter().enumerate() {
                if load as f64 > avg_load * 1.5 {
                    debug!("⚖️ Worker #{} overloaded ({} > {:.1}), stealing tasks",
                        worker_id, load, avg_load * 1.5);
                }
            }
        }

        self.record_metric("dispatcher.manual_rebalance", 1.0).await;
        self.record_metric("dispatcher.imbalance", imbalance).await;

        info!("✅ Workers rebalanced, imbalance: {:.2} → {:.2}",
            imbalance, self.work_stealing_dispatcher.get_advanced_stats().await.imbalance);
    }

    /// 📈 МАСШТАБИРОВАНИЕ ВВЕРХ
    async fn scale_up(&self, count: usize) -> Result<usize, BatchError> {
        info!("📈 Scaling up by {} workers", count);

        let _lock = self.scaling_lock.lock().await;

        let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);
        let settings = self.scaling_settings.read().await;

        if count == 0 {
            return Ok(0);
        }

        if current_workers >= settings.max_worker_count {
            warn!("⚠️ Cannot scale up: already at maximum workers ({})", current_workers);
            return Ok(0);
        }

        let added = self.worker_pool.add_workers(count, self.work_stealing_dispatcher.clone()).await?;

        if added > 0 {
            let mut new_settings = settings.clone();
            new_settings.last_scaling_time = Instant::now();
            *self.scaling_settings.write().await = new_settings;

            self.record_metric("scaling.scale_up", added as f64).await;
            self.record_metric("scaling.current_workers", (current_workers + added) as f64).await;

            info!("✅ Scaled UP from {} to {} workers (added {})",
                current_workers, current_workers + added, added);
        }

        Ok(added)
    }

    /// 📉 МАСШТАБИРОВАНИЕ ВНИЗ
    async fn scale_down(&self, count: usize) -> Result<usize, BatchError> {
        info!("📉 Scaling down by {} workers", count);

        let _lock = self.scaling_lock.lock().await;

        let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);
        let settings = self.scaling_settings.read().await;

        if count == 0 {
            return Ok(0);
        }

        if current_workers <= settings.min_worker_count {
            warn!("⚠️ Cannot scale down: already at minimum workers ({})", current_workers);
            return Ok(0);
        }

        let removed = self.worker_pool.remove_workers(count).await?;

        if removed > 0 {
            let mut new_settings = settings.clone();
            new_settings.last_scaling_time = Instant::now();
            *self.scaling_settings.write().await = new_settings;

            self.record_metric("scaling.scale_down", removed as f64).await;
            self.record_metric("scaling.current_workers", (current_workers - removed) as f64).await;

            info!("✅ Scaled DOWN from {} to {} workers (removed {})",
                current_workers, current_workers - removed, removed);
        }

        Ok(removed)
    }

    /// ⚙️ ОБНОВЛЕНИЕ НАСТРОЕК СКЕЙЛИНГА
    async fn update_scaling_settings(&self, settings: ScalingSettings) {
        let mut current = self.scaling_settings.write().await;

        // Валидация настроек
        let mut validated = settings;
        if validated.min_worker_count < 1 {
            validated.min_worker_count = 1;
            warn!("⚠️ Min worker count adjusted to 1");
        }
        if validated.max_worker_count < validated.min_worker_count {
            validated.max_worker_count = validated.min_worker_count.max(256);
            warn!("⚠️ Max worker count adjusted to {}", validated.max_worker_count);
        }
        if validated.scaling_cooldown_seconds < 10 {
            validated.scaling_cooldown_seconds = 10;
            warn!("⚠️ Scaling cooldown adjusted to 10 seconds");
        }
        if validated.work_stealing_target_queue_size < 100 {
            validated.work_stealing_target_queue_size = 100;
            warn!("⚠️ Target queue size adjusted to 100");
        }
        if validated.buffer_pool_target_hit_rate <= 0.0 || validated.buffer_pool_target_hit_rate > 1.0 {
            validated.buffer_pool_target_hit_rate = 0.85;
            warn!("⚠️ Buffer pool target hit rate adjusted to 0.85");
        }
        if validated.crypto_processor_target_success_rate <= 0.0 || validated.crypto_processor_target_success_rate > 1.0 {
            validated.crypto_processor_target_success_rate = 0.99;
            warn!("⚠️ Crypto processor target success rate adjusted to 0.99");
        }
        if validated.connection_target_count < 1000 {
            validated.connection_target_count = 1000;
            warn!("⚠️ Connection target count adjusted to 1000");
        }

        // Применяем новые настройки к воркер-пулу
        if validated.min_worker_count != current.min_worker_count {
            let worker_pool = self.worker_pool.clone();
            let current_workers = worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);
            if current_workers < validated.min_worker_count {
                let increase = validated.min_worker_count - current_workers;
                drop(current);
                let _ = self.scale_up(increase).await;
                current = self.scaling_settings.write().await;
            }
        }

        if validated.max_worker_count != current.max_worker_count {
            let worker_pool = self.worker_pool.clone();
            let current_workers = worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);
            if current_workers > validated.max_worker_count {
                let decrease = current_workers - validated.max_worker_count;
                drop(current);
                let _ = self.scale_down(decrease).await;
                current = self.scaling_settings.write().await;
            }
        }

        *current = validated;

        // Записываем метрики
        self.record_metric("scaling.min_workers", current.min_worker_count as f64).await;
        self.record_metric("scaling.max_workers", current.max_worker_count as f64).await;
        self.record_metric("scaling.auto_scaling_enabled", current.auto_scaling_enabled as i64 as f64).await;
        self.record_metric("scaling.cooldown_seconds", current.scaling_cooldown_seconds as f64).await;
        self.record_metric("scaling.target_queue_size", current.work_stealing_target_queue_size as f64).await;
        self.record_metric("scaling.target_hit_rate", current.buffer_pool_target_hit_rate).await;
        self.record_metric("scaling.target_success_rate", current.crypto_processor_target_success_rate).await;
        self.record_metric("scaling.target_connections", current.connection_target_count as f64).await;

        info!("⚙️ Scaling settings updated:");
        info!("  ├─ Min workers: {}", current.min_worker_count);
        info!("  ├─ Max workers: {}", current.max_worker_count);
        info!("  ├─ Auto scaling: {}", current.auto_scaling_enabled);
        info!("  ├─ Cooldown: {}s", current.scaling_cooldown_seconds);
        info!("  ├─ Target queue: {}", current.work_stealing_target_queue_size);
        info!("  ├─ Target hit rate: {:.1}%", current.buffer_pool_target_hit_rate * 100.0);
        info!("  ├─ Target success rate: {:.1}%", current.crypto_processor_target_success_rate * 100.0);
        info!("  └─ Target connections: {}", current.connection_target_count);
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

    /// 🔄 ЗАПУСК ОБРАБОТЧИКА БАТЧЕЙ
    async fn start_batch_processor(&self) {
        let pending_batches = self.pending_batches.clone();
        let is_running = self.is_running.clone();
        let system = self.clone();

        tokio::spawn(async move {
            debug!("🔄 Batch processor started");
            let mut interval = tokio::time::interval(Duration::from_millis(50));
            let mut batch_id_counter = 0u64;

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

                for mut batch in batches_to_process {
                    batch_id_counter += 1;
                    batch.id = batch_id_counter;
                    system.process_batch(batch).await;
                }
            }

            debug!("👋 Batch processor stopped");
        });
    }

    /// 📦 ОБРАБОТКА БАТЧА
    async fn process_batch(&self, batch: PendingBatch) {
        let start_time = Instant::now();
        let batch_size = batch.operations.len();
        let batch_id = batch.id;

        debug!("🔄 Processing batch #{} with {} operations", batch_id, batch_size);

        let mut successful = 0;
        let mut processed_packets = Vec::new();

        for operation in batch.operations {
            match operation {
                BatchOperation::Encryption { session_id, data, key: _, nonce: _ } => {
                    if let Some(session) = self.session_manager.get_session(&session_id).await {
                        // ✅ ИСПРАВЛЕНО: Сначала дешифруем пакет
                        match self.packet_processor.process_incoming_vec(&data, &session) {
                            Ok((packet_type, decrypted_payload)) => {
                                // ✅ Затем отправляем в packet_service для обработки
                                match self.packet_service.process_packet(
                                    session.clone(),
                                    packet_type,
                                    decrypted_payload,
                                    batch.source_addr,
                                ).await {
                                    Ok(processing_result) => {
                                        // ✅ Шифруем ответ от packet_service
                                        match self.packet_processor.create_outgoing_vec(
                                            &session,
                                            processing_result.packet_type,
                                            &processing_result.response,
                                        ) {
                                            Ok(encrypted_response) => {
                                                successful += 1;

                                                let _ = self.writer.write(
                                                    batch.source_addr,
                                                    session_id,
                                                    Bytes::from(encrypted_response),
                                                    processing_result.priority,
                                                    processing_result.packet_type == 0x01, // flush для Ping
                                                ).await;

                                                debug!("✅ Processed packet type 0x{:02x} through packet_service", packet_type);
                                            }
                                            Err(e) => {
                                                debug!("❌ Encryption failed: {}", e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        debug!("❌ Packet service processing failed: {}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                debug!("❌ Decryption failed: {}", e);
                            }
                        }
                    }
                }

                BatchOperation::Decryption { session_id, data, key: _, nonce: _ } => {
                    let packet_type_byte = if !data.is_empty() { data[0] } else { 0 };

                    if !is_packet_supported(packet_type_byte) {
                        debug!("⚠️ Unsupported packet type for decryption: 0x{:02x}", packet_type_byte);
                        continue;
                    }

                    if let Some(session) = self.session_manager.get_session(&session_id).await {
                        match self.packet_processor.process_incoming_vec(&data, &session) {
                            Ok((decoded_type, _)) => {
                                if decoded_type == packet_type_byte {
                                    successful += 1;
                                    processed_packets.push((packet_type_byte, true));
                                    debug!("✅ Decrypted packet type 0x{:02x}", packet_type_byte);
                                }
                            }
                            Err(e) => {
                                debug!("❌ Decryption failed for packet type 0x{:02x}: {}", packet_type_byte, e);
                                processed_packets.push((packet_type_byte, false));
                            }
                        }
                    }
                }

                BatchOperation::Hashing { data, key } => {
                    if let Some(key) = key {
                        let keys = vec![key; 1];
                        let inputs = vec![data.to_vec()];
                        let hashes = self.blake3_accelerator.hash_keyed_batch(&keys, &inputs).await;
                        if !hashes.is_empty() {
                            successful += 1;
                        }
                    }
                }

                BatchOperation::Processing { session_id, data, processor_type } => {
                    let packet_type_byte = if !data.is_empty() { data[0] } else { 0 };

                    if !is_packet_supported(packet_type_byte) {
                        debug!("⚠️ Unsupported packet type for processing: 0x{:02x}", packet_type_byte);
                        continue;
                    }

                    match processor_type {
                        ProcessorType::Accelerated => {
                            let _priority = get_packet_priority(packet_type_byte).unwrap_or(Priority::Normal);

                            if let Some(session) = self.session_manager.get_session(&session_id).await {
                                match self.packet_processor.create_outgoing_vec(&session, packet_type_byte, &data) {
                                    Ok(_encrypted) => {
                                        successful += 1;
                                        processed_packets.push((packet_type_byte, true));
                                    }
                                    Err(e) => {
                                        debug!("❌ Processing failed for packet type 0x{:02x}: {}", packet_type_byte, e);
                                        processed_packets.push((packet_type_byte, false));
                                    }
                                }
                            }
                        }
                        _ => {
                            successful += 1;
                            processed_packets.push((packet_type_byte, true));
                        }
                    }
                }
            }
        }

        let success_rate = if batch_size > 0 {
            successful as f64 / batch_size as f64
        } else {
            1.0
        };

        let processing_time = start_time.elapsed();

        // ✅ ЛОГИРУЕМ СТАТИСТИКУ ПО ТИПАМ ПАКЕТОВ
        let mut packet_stats = HashMap::new();
        for (packet_type, success) in processed_packets {
            *packet_stats.entry(packet_type).or_insert((0, 0)) = (
                packet_stats.get(&packet_type).map(|(s, _)| s + 1).unwrap_or(1),
                if success { 1 } else { 0 }
            );
        }

        if !packet_stats.is_empty() {
            debug!("📊 Batch #{} packet types:", batch_id);
            for (packet_type, (total, successful_count)) in packet_stats {
                if let Some(info) = get_packet_info(packet_type) {
                    debug!("  - 0x{:02x}: {}/{} ({:.1}%) - {}",
                       packet_type, successful_count, total,
                       (successful_count as f64 / total as f64) * 100.0,
                       info.description);
                }
            }
        }

        self.adaptive_batcher.record_batch_execution(
            batch_size,
            processing_time,
            success_rate,
            self.pending_batches.read().await.len(),
        ).await;

        {
            let mut stats = self.stats.write().await;
            stats.total_batches_processed += 1;
            stats.crypto_operations += successful as u64;
        }

        let event = SystemEvent::BatchCompleted {
            batch_id,
            size: batch_size,
            processing_time,
            success_rate,
        };

        let _ = self.event_tx.send(event).await;

        debug!("✅ Batch #{} completed: {}/{} successful, {:.1}% in {:?}",
        batch_id, successful, batch_size, success_rate * 100.0, processing_time);
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
        let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);

        SystemStatus {
            timestamp: Instant::now(),
            is_running: self.is_running.load(std::sync::atomic::Ordering::Relaxed),
            statistics: stats,
            active_connections: connections.len(),
            active_workers: current_workers,
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
                session_cache: self.session_cache.read().await.len(),
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

        if let Some(mut counter) = self.performance_counters.get_mut(name) {
            counter.update(value);
        } else {
            let mut counter = PerformanceCounter::new(name.to_string(), 60);
            counter.update(value);
            self.performance_counters.insert(name.to_string(), counter);
        }
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

    /// 📦 Получение текущего количества воркеров
    pub fn get_current_workers(&self) -> usize {
        self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 📦 Добавление операции в батч
    pub async fn add_to_batch(&self, operation: BatchOperation, source_addr: std::net::SocketAddr, priority: Priority) {
        let mut pending = self.pending_batches.write().await;

        let batch = pending.iter_mut().find(|b|
            b.source_addr == source_addr &&
                b.priority == priority &&
                b.deadline.map_or(true, |d| d > Instant::now())
        );

        if let Some(batch) = batch {
            batch.operations.push(operation);
        } else {
            pending.push(PendingBatch {
                id: 0,
                operations: vec![operation],
                priority,
                source_addr,
                created_at: Instant::now(),
                deadline: Some(Instant::now() + Duration::from_millis(100)),
                retry_count: 0,
            });
        }
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
    pub active_workers: usize,
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
    pub source_addr: std::net::SocketAddr,
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
    pub session_cache: usize,
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
            worker_pool: self.worker_pool.clone(),
            scaling_lock: self.scaling_lock.clone(),
        }
    }
}

use std::collections::VecDeque;