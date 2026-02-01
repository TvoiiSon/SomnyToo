use std::sync::Arc;
use std::collections::HashMap;
use std::time::{Instant, Duration};
use tokio::sync::{mpsc, RwLock, Mutex, Semaphore};
use tokio::task::JoinSet;
use tracing::{info, debug, warn, error, trace};

use super::config::OrchestratorConfig;
use super::stats::OrchestratorStats;
use crate::core::protocol::phantom_crypto::batch::processor::crypto_batch_processor::{CryptoBatchProcessor, CryptoBatch};
use crate::core::protocol::phantom_crypto::batch::processor::operation::CryptoOperation;
use crate::core::protocol::phantom_crypto::batch::types::priority::BatchPriority;
use crate::core::protocol::phantom_crypto::batch::types::state::{BatchState, BatchStatus};
use crate::core::protocol::phantom_crypto::batch::types::error::BatchError;
use crate::core::protocol::phantom_crypto::batch::types::result::BatchResult;
use crate::core::protocol::phantom_crypto::core::keys::PhantomSession;
use crate::core::monitoring::unified_monitor::{UnifiedMonitor, AlertLevel};

/// События в batch pipeline
#[derive(Debug)]
pub enum BatchEvent {
    NewOperation {
        session_id: Vec<u8>,
        operation: CryptoOperation,
        priority: BatchPriority,
        timestamp: Instant,
    },
    FlushBatch {
        priority: BatchPriority,
        force: bool,
    },
    BatchCompleted {
        batch_id: u64,
        result: BatchResult,
    },
    EmergencyFlushAll,
    Shutdown,
}

/// Оркестратор пакетной обработки
pub struct BatchOrchestrator {
    // Компоненты
    crypto_processor: Arc<CryptoBatchProcessor>,
    session_registry: Arc<RwLock<HashMap<Vec<u8>, Arc<PhantomSession>>>>,
    monitor: Arc<UnifiedMonitor>,

    // Очереди и батчи
    pending_batches: Arc<Mutex<Vec<(BatchPriority, CryptoBatch)>>>,
    batch_states: Arc<RwLock<HashMap<u64, BatchState>>>,
    completed_batches: mpsc::Sender<BatchResult>,

    // Каналы коммуникации
    event_tx: mpsc::Sender<BatchEvent>,
    event_rx: Mutex<mpsc::Receiver<BatchEvent>>,

    // Конфигурация
    config: OrchestratorConfig,

    // Статистика и контроль
    stats: Mutex<OrchestratorStats>,
    flush_timers: Arc<RwLock<HashMap<BatchPriority, tokio::time::Interval>>>,
    backpressure_semaphore: Arc<Semaphore>,

    // Worker задачи
    workers: JoinSet<()>,

    // Счетчики
    batch_counter: std::sync::atomic::AtomicU64,
    flush_counter: std::sync::atomic::AtomicU64,
}

impl BatchOrchestrator {
    /// Создание нового оркестратора
    pub async fn new(
        crypto_config: crate::core::protocol::phantom_crypto::batch::processor::config::BatchCryptoConfig,
        orchestrator_config: OrchestratorConfig,
        monitor: Arc<UnifiedMonitor>,
    ) -> Self {
        let crypto_processor = Arc::new(CryptoBatchProcessor::new(crypto_config));
        let (event_tx, event_rx) = mpsc::channel(orchestrator_config.max_queue_size);
        let (completed_tx, _completed_rx) = mpsc::channel(1000);

        let mut flush_timers_map = HashMap::new();
        for (priority, interval) in &orchestrator_config.flush_intervals {
            let mut timer = tokio::time::interval(*interval);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            flush_timers_map.insert(*priority, timer);
        }

        let mut orchestrator = Self {
            crypto_processor: crypto_processor.clone(),
            session_registry: Arc::new(RwLock::new(HashMap::new())),
            monitor: monitor.clone(),
            pending_batches: Arc::new(Mutex::new(Vec::new())),
            batch_states: Arc::new(RwLock::new(HashMap::new())),
            completed_batches: completed_tx,
            event_tx: event_tx.clone(),
            event_rx: Mutex::new(event_rx),
            config: orchestrator_config.clone(),
            stats: Mutex::new(OrchestratorStats::new()),
            flush_timers: Arc::new(RwLock::new(flush_timers_map)),
            backpressure_semaphore: Arc::new(Semaphore::new(
                orchestrator_config.backpressure_threshold
            )),
            workers: JoinSet::new(),
            batch_counter: std::sync::atomic::AtomicU64::new(0),
            flush_counter: std::sync::atomic::AtomicU64::new(0),
        };

        // Запускаем worker-ов
        for worker_id in 0..orchestrator_config.worker_count {
            orchestrator.spawn_worker(worker_id).await;
        }

        // Запускаем flush timers
        orchestrator.start_flush_timers().await;

        // Запускаем мониторинг таймаутов
        orchestrator.start_timeout_monitor().await;

        info!("🚀 BatchOrchestrator initialized with {} workers",
              orchestrator_config.worker_count);

        orchestrator
    }

    /// Регистрация сессии
    pub async fn register_session(&self, session_id: Vec<u8>, session: Arc<PhantomSession>) {
        let session_id_clone = session_id.clone();
        let mut registry = self.session_registry.write().await;
        registry.insert(session_id, session);
        debug!("Registered session in batch orchestrator: {}", hex::encode(&session_id_clone));

        // Обновляем статистику
        let mut stats = self.stats.lock().await;
        stats.update_session_registry_size(registry.len());
    }

    /// Отправка операции на обработку
    pub async fn submit_operation(
        &self,
        session_id: Vec<u8>,
        operation: CryptoOperation,
        priority: BatchPriority,
    ) -> Result<(), BatchError> {
        let start = Instant::now();

        // Проверяем backpressure
        let permit = match self.backpressure_semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let mut stats = self.stats.lock().await;
                stats.register_backpressure_event();

                // Аварийный flush если очередь переполнена
                if self.should_emergency_flush().await {
                    if let Err(e) = self.event_tx.send(BatchEvent::EmergencyFlushAll).await {
                        return Err(BatchError::ChannelError(e.to_string()));
                    }
                }

                return Err(BatchError::Backpressure);
            }
        };

        let event = BatchEvent::NewOperation {
            session_id,
            operation,
            priority,
            timestamp: Instant::now(),
        };

        if let Err(e) = self.event_tx.send(event).await {
            drop(permit); // Освобождаем permit при ошибке
            return Err(BatchError::ChannelError(e.to_string()));
        }

        // Обновляем статистику
        let mut stats = self.stats.lock().await;
        stats.total_operations += 1;

        debug!("Operation submitted in {:?}", start.elapsed());

        Ok(())
    }

    /// Принудительный flush батча
    pub async fn flush_batch(&self, priority: BatchPriority, force: bool) -> Result<(), BatchError> {
        self.flush_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if let Err(e) = self.event_tx.send(BatchEvent::FlushBatch { priority, force }).await {
            return Err(BatchError::ChannelError(e.to_string()));
        }

        // Обновляем статистику
        let mut stats = self.stats.lock().await;
        stats.register_flush();

        Ok(())
    }

    /// Аварийный flush всех батчей
    pub async fn emergency_flush_all(&self) -> Result<(), BatchError> {
        let mut stats = self.stats.lock().await;
        stats.register_emergency_flush();

        if let Err(e) = self.event_tx.send(BatchEvent::EmergencyFlushAll).await {
            return Err(BatchError::ChannelError(e.to_string()));
        }
        Ok(())
    }

    /// Запуск worker задачи
    async fn spawn_worker(&mut self, worker_id: usize) {
        let orchestrator = self.clone();

        self.workers.spawn(async move {
            info!("👷 Batch worker #{} started", worker_id);

            while let Some(event) = orchestrator.receive_event().await {
                match event {
                    BatchEvent::NewOperation { session_id, operation, priority, timestamp } => {
                        orchestrator.handle_new_operation(
                            worker_id, session_id, operation, priority, timestamp
                        ).await;
                    }
                    BatchEvent::FlushBatch { priority, force } => {
                        orchestrator.handle_flush_batch(worker_id, priority, force).await;
                    }
                    BatchEvent::BatchCompleted { batch_id, result } => {
                        orchestrator.handle_batch_completed(batch_id, result).await;
                    }
                    BatchEvent::EmergencyFlushAll => {
                        orchestrator.handle_emergency_flush(worker_id).await;
                    }
                    BatchEvent::Shutdown => {
                        info!("👷 Batch worker #{} shutting down", worker_id);
                        break;
                    }
                }
            }
        });
    }

    /// Обработка новой операции
    async fn handle_new_operation(
        &self,
        worker_id: usize,
        session_id: Vec<u8>,
        operation: CryptoOperation,
        priority: BatchPriority,
        _timestamp: Instant,
    ) {
        trace!("Worker #{}: New {:?} operation for session {}",
               worker_id, priority, hex::encode(&session_id));

        let mut pending_batches = self.pending_batches.lock().await;

        // Ищем батч для этого приоритета
        let batch_index = pending_batches.iter_mut()
            .position(|(p, _)| *p == priority);

        if let Some(index) = batch_index {
            let (_, batch) = &mut pending_batches[index];
            batch.add_operation(operation);

            // Проверяем, нужно ли flush
            if batch.len() >= self.config.max_batch_size {
                self.flush_batch_internal(worker_id, priority, false).await;
            }
        } else {
            // Создаем новый батч
            let batch_id = self.batch_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut batch = CryptoBatch::new(
                batch_id,
                self.config.max_batch_size,
                priority
            );
            batch.add_operation(operation);

            // Создаем состояние батча
            let batch_state = BatchState::new(batch_id, priority, 1);
            {
                let mut batch_states = self.batch_states.write().await;
                batch_states.insert(batch_id, batch_state);
            }

            pending_batches.push((priority, batch));
        }

        // Обновляем статистику очереди
        self.update_queue_stats(&pending_batches).await;

        // Проверяем emergency flush
        if self.should_emergency_flush().await {
            if let Err(e) = self.event_tx.send(BatchEvent::EmergencyFlushAll).await {
                error!("Failed to send emergency flush: {}", e);
            }
        }
    }

    /// Обработка flush батча
    async fn handle_flush_batch(&self, worker_id: usize, priority: BatchPriority, force: bool) {
        self.flush_batch_internal(worker_id, priority, force).await;
    }

    /// Внутренний метод flush батча
    async fn flush_batch_internal(&self, worker_id: usize, priority: BatchPriority, _force: bool) {
        let mut pending_batches = self.pending_batches.lock().await;

        // Ищем и удаляем батч для этого приоритета
        let batch_index = pending_batches.iter()
            .position(|(p, _)| *p == priority);

        if let Some(index) = batch_index {
            let (_, batch) = pending_batches.remove(index);

            if batch.is_empty() {
                trace!("Worker #{}: Empty batch for priority {:?}, skipping", worker_id, priority);
                return;
            }

            let batch_id = batch.id;
            let batch_size = batch.len();

            // Обновляем состояние батча
            {
                let mut batch_states = self.batch_states.write().await;
                if let Some(state) = batch_states.get_mut(&batch_id) {
                    state.start_processing();
                    state.size = batch_size;
                } else {
                    // Создаем новое состояние если не существует
                    let mut batch_state = BatchState::new(batch_id, priority, batch_size);
                    batch_state.start_processing();
                    batch_states.insert(batch_id, batch_state);
                }
            }

            // Запускаем обработку батча
            self.process_batch_async(worker_id, batch).await;

            debug!("Worker #{}: Flushed {:?} batch #{} with {} operations",
                   worker_id, priority, batch_id, batch_size);
        }
    }

    /// Асинхронная обработка батча
    async fn process_batch_async(&self, worker_id: usize, batch: CryptoBatch) {
        let batch_id = batch.id;
        let priority = batch.priority;
        let crypto_processor = self.crypto_processor.clone();
        let session_registry = self.session_registry.clone();
        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            let start = Instant::now();

            // Получаем сессии для этого батча
            let sessions = {
                let registry = session_registry.read().await;
                let mut batch_sessions = HashMap::new();

                for op in &batch.operations {
                    let session_id = match op {
                        CryptoOperation::Encrypt { session_id, .. } => session_id,
                        CryptoOperation::Decrypt { session_id, .. } => session_id,
                    };

                    if let Some(session) = registry.get(session_id) {
                        batch_sessions.insert(session_id.clone(), session.clone());
                    }
                }

                batch_sessions
            };

            // Обрабатываем батч
            let result = if matches!(priority, BatchPriority::Realtime | BatchPriority::High) {
                // Real-time батчи обрабатываем немедленно
                crypto_processor.process_decryption_batch(batch, &sessions).await
            } else {
                // Остальные - с возможностью параллелизма
                crypto_processor.process_encryption_batch(batch, &sessions).await
            };

            let processing_time = start.elapsed();

            // Отправляем результат
            if let Err(e) = event_tx.send(BatchEvent::BatchCompleted { batch_id, result }).await {
                error!("Failed to send batch completion for batch #{}: {}", batch_id, e);
            }

            trace!("Worker #{}: Batch #{} processed in {:?}", worker_id, batch_id, processing_time);
        });
    }

    /// Обработка завершенного батча
    async fn handle_batch_completed(&self, batch_id: u64, result: BatchResult) {
        // Обновляем состояние батча
        let success = result.failed == 0;
        {
            let mut batch_states = self.batch_states.write().await;
            if let Some(state) = batch_states.get_mut(&batch_id) {
                state.complete(success);
            }
        }

        // Обновляем статистику
        {
            let mut stats = self.stats.lock().await;
            stats.total_batches += 1;
            if success {
                stats.register_successful_batch();
            } else {
                stats.register_failed_batch();
            }

            stats.update_operations(result.successful + result.failed, result.processing_time);
        }

        // Отправляем результаты дальше
        let result_clone = BatchResult {
            batch_id: result.batch_id,
            results: result.results.clone(),
            processing_time: result.processing_time,
            successful: result.successful,
            failed: result.failed,
            simd_utilization: result.simd_utilization,
        };

        if let Err(e) = self.completed_batches.send(result_clone).await {
            warn!("Failed to forward batch result for batch #{}: {}", batch_id, e);
        }

        // Освобождаем backpressure permits
        self.backpressure_semaphore.add_permits(result.successful + result.failed);

        // Обновляем мониторинг
        self.update_monitoring(batch_id, &result).await;
    }

    /// Обработка аварийного flush
    async fn handle_emergency_flush(&self, worker_id: usize) {
        warn!("Worker #{}: Performing emergency flush of all batches", worker_id);

        let mut pending_batches = self.pending_batches.lock().await;
        let batches_to_process = std::mem::take(&mut *pending_batches);

        for (priority, batch) in batches_to_process {
            if !batch.is_empty() {
                debug!("Worker #{}: Emergency flushing {:?} batch with {} operations",
                   worker_id, priority, batch.len());
                self.process_batch_async(worker_id, batch).await;
            }
        }

        // Сбрасываем backpressure
        self.backpressure_semaphore.add_permits(self.config.backpressure_threshold);
    }

    /// Запуск таймеров для автоматического flush
    async fn start_flush_timers(&self) {
        let event_tx = self.event_tx.clone();
        let flush_timers = self.flush_timers.clone();

        // Для каждого приоритета запускаем отдельный таймер
        let timers = {
            let flush_timers_guard = flush_timers.read().await;
            flush_timers_guard.iter()
                .map(|(priority, timer)| {
                    let mut new_timer = tokio::time::interval(timer.period());
                    new_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    (*priority, new_timer)
                })
                .collect::<HashMap<BatchPriority, tokio::time::Interval>>()
        };

        for (priority, mut timer) in timers {
            let event_tx = event_tx.clone();

            tokio::spawn(async move {
                loop {
                    timer.tick().await;

                    if let Err(e) = event_tx.send(BatchEvent::FlushBatch {
                        priority,
                        force: false,
                    }).await {
                        error!("Failed to send flush event for {:?}: {}", priority, e);
                        break;
                    }
                }
            });
        }
    }

    /// Запуск мониторинга таймаутов
    async fn start_timeout_monitor(&self) {
        let orchestrator = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));

            loop {
                interval.tick().await;
                orchestrator.check_timeouts().await;
            }
        });
    }

    /// Проверка таймаутов батчей
    async fn check_timeouts(&self) {
        let batch_states = self.batch_states.read().await;
        let mut timed_out = Vec::new();

        for (batch_id, state) in batch_states.iter() {
            if state.is_timed_out(self.config.batch_timeout) && state.status == BatchStatus::Processing {
                timed_out.push(*batch_id);
            }
        }

        if !timed_out.is_empty() {
            warn!("Found {} timed out batches", timed_out.len());

            // Обновляем статистику
            {
                let mut stats = self.stats.lock().await;
                stats.batch_timeouts += timed_out.len() as u64;
            }
        }
    }

    /// Проверка необходимости emergency flush
    async fn should_emergency_flush(&self) -> bool {
        let pending_batches = self.pending_batches.lock().await;
        let total_operations: usize = pending_batches.iter().map(|(_, b)| b.len()).sum();
        total_operations >= self.config.emergency_flush_threshold
    }

    /// Обновление статистики очереди
    async fn update_queue_stats(&self, pending_batches: &Vec<(BatchPriority, CryptoBatch)>) {
        let mut stats = self.stats.lock().await;
        let queue_sizes: Vec<usize> = pending_batches.iter().map(|(_, b)| b.len()).collect();
        stats.update_queue_sizes(queue_sizes);
    }

    /// Обновление мониторинга
    async fn update_monitoring(&self, batch_id: u64, result: &BatchResult) {
        let alert_level = if result.failed > 0 {
            AlertLevel::Warning
        } else {
            AlertLevel::Info
        };

        // Отправляем алерт в мониторинг
        self.monitor.add_alert(
            alert_level,
            "batch_processing",
            &format!("Batch #{} completed: {}/{} successful, time: {:?}, SIMD: {:.1}%",
                     batch_id,
                     result.successful,
                     result.successful + result.failed,
                     result.processing_time,
                     result.simd_utilization)
        ).await;

        debug!("Batch #{} completed: {}/{} successful, time: {:?}, SIMD: {:.1}%",
               batch_id,
               result.successful,
               result.successful + result.failed,
               result.processing_time,
               result.simd_utilization);
    }

    /// Получение события из очереди
    async fn receive_event(&self) -> Option<BatchEvent> {
        let mut event_rx = self.event_rx.lock().await;
        event_rx.recv().await
    }

    /// Получение статистики
    pub async fn get_stats(&self) -> OrchestratorStats {
        let stats = self.stats.lock().await.clone();

        // Обновляем дополнительные метрики
        let session_registry_size = {
            let registry = self.session_registry.read().await;
            registry.len()
        };

        let mut stats = stats;
        stats.update_session_registry_size(session_registry_size);
        stats.update_active_workers(self.workers.len());

        // Оцениваем использование памяти (очень грубая оценка)
        let memory_estimate = session_registry_size * 1024; // Примерная оценка
        stats.update_memory_usage(memory_estimate);

        stats
    }

    /// Получение состояния батча
    pub async fn get_batch_state(&self, batch_id: u64) -> Option<BatchState> {
        self.batch_states.read().await.get(&batch_id).cloned()
    }

    /// Получение всех состояний батчей
    pub async fn get_all_batch_states(&self) -> Vec<BatchState> {
        self.batch_states.read().await.values().cloned().collect()
    }

    /// Получение счетчика flush
    pub fn get_flush_count(&self) -> u64 {
        self.flush_counter.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Получение таймеров flush
    pub async fn get_flush_timers(&self) -> HashMap<BatchPriority, Duration> {
        let timers = self.flush_timers.read().await;
        timers.iter()
            .map(|(priority, timer)| (*priority, timer.period()))
            .collect()
    }

    /// Получение размера регистра сессий
    pub async fn session_registry_size(&self) -> usize {
        let registry = self.session_registry.read().await;
        registry.len()
    }

    /// Получение количества ожидающих батчей
    pub async fn pending_batches_count(&self) -> usize {
        let pending_batches = self.pending_batches.lock().await;
        pending_batches.len()
    }

    /// Получение общего количества операций в очереди
    pub async fn queued_operations_count(&self) -> usize {
        let pending_batches = self.pending_batches.lock().await;
        pending_batches.iter().map(|(_, batch)| batch.len()).sum()
    }

    /// Получение доступных backpressure permits
    pub fn available_backpressure_permits(&self) -> usize {
        self.backpressure_semaphore.available_permits()
    }

    /// Остановка оркестратора
    pub async fn shutdown(&mut self) {
        // Отправляем событие shutdown всем worker-ам
        for _ in 0..self.config.worker_count {
            if let Err(e) = self.event_tx.send(BatchEvent::Shutdown).await {
                error!("Failed to send shutdown: {}", e);
            }
        }

        // Ждем завершения worker-ов
        while let Some(result) = self.workers.join_next().await {
            if let Err(e) = result {
                error!("Worker failed to shutdown: {}", e);
            }
        }

        // Аварийный flush оставшихся батчей
        let _ = self.emergency_flush_all().await;

        info!("BatchOrchestrator shutdown complete");
    }

    /// Получение конфигурации
    pub fn get_config(&self) -> &OrchestratorConfig {
        &self.config
    }

    /// Получение криптопроцессора
    pub fn get_crypto_processor(&self) -> Arc<CryptoBatchProcessor> {
        self.crypto_processor.clone()
    }

    /// Логирование статистики
    pub async fn log_stats(&self) {
        let stats = self.get_stats().await;
        info!("📊 BatchOrchestrator Stats: {}", stats.to_log_string());
    }
}

impl Clone for BatchOrchestrator {
    fn clone(&self) -> Self {
        // Создаем новые event channels
        let (event_tx, event_rx) = mpsc::channel(self.config.max_queue_size);

        // Клонируем flush timers
        let mut flush_timers = HashMap::new();
        for (priority, interval) in &self.config.flush_intervals {
            let mut timer = tokio::time::interval(*interval);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            flush_timers.insert(*priority, timer);
        }

        Self {
            crypto_processor: self.crypto_processor.clone(),
            session_registry: Arc::new(RwLock::new(HashMap::new())),
            monitor: self.monitor.clone(),
            pending_batches: Arc::new(Mutex::new(Vec::new())),
            batch_states: Arc::new(RwLock::new(HashMap::new())),
            completed_batches: self.completed_batches.clone(),
            event_tx,
            event_rx: Mutex::new(event_rx),
            config: self.config.clone(),
            stats: Mutex::new(OrchestratorStats::new()),
            flush_timers: Arc::new(RwLock::new(flush_timers)),
            backpressure_semaphore: self.backpressure_semaphore.clone(),
            workers: JoinSet::new(),
            batch_counter: std::sync::atomic::AtomicU64::new(0),
            flush_counter: std::sync::atomic::AtomicU64::new(0),
        }
    }
}