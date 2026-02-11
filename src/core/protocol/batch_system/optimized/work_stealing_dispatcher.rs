use std::sync::Arc;
use std::time::{Instant, Duration};
use dashmap::DashMap;
use tracing::{info, debug, error};
use bytes::Bytes;
use flume::{Sender, Receiver, bounded};
use tokio::sync::Semaphore;

use crate::core::protocol::batch_system::types::error::BatchError;
use crate::core::protocol::batch_system::types::priority::Priority;
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;

use crate::core::protocol::batch_system::adaptive_batcher::{AdaptiveBatcher, BatchMetrics};
use crate::core::protocol::batch_system::qos_manager::{QosManager};
use crate::core::protocol::batch_system::circuit_breaker::{CircuitBreaker, CircuitState};

/// ⚡ Задача для work-stealing диспетчера
#[derive(Debug, Clone)]
pub struct WorkStealingTask {
    pub id: u64,
    pub session_id: Vec<u8>,
    pub data: Bytes,
    pub source_addr: std::net::SocketAddr,
    pub priority: Priority,
    pub created_at: Instant,
    pub worker_id: Option<usize>,
    pub retry_count: u8,
    pub deadline: Option<Instant>,
}

impl Default for WorkStealingTask {
    fn default() -> Self {
        Self {
            id: 0,
            session_id: Vec::new(),
            data: Bytes::new(),
            source_addr: "0.0.0.0:0".parse().unwrap(),
            priority: Priority::Normal,
            created_at: Instant::now(),
            worker_id: None,
            retry_count: 0,
            deadline: None,
        }
    }
}

impl WorkStealingTask {
    pub fn is_expired(&self) -> bool {
        self.deadline.map_or(false, |d| Instant::now() > d)
    }

    pub fn with_deadline(mut self, timeout: Duration) -> Self {
        self.deadline = Some(Instant::now() + timeout);
        self
    }

    pub fn with_retry(mut self) -> Self {
        self.retry_count += 1;
        self
    }
}

/// 📊 Результат обработки
#[derive(Debug, Clone)]
pub struct WorkStealingResult {
    pub task_id: u64,
    pub session_id: Vec<u8>,
    pub result: Result<Vec<u8>, String>,
    pub processing_time: Duration,
    pub worker_id: usize,
    pub destination_addr: std::net::SocketAddr,
    pub completed_at: Instant,
}

/// 📈 Расширенная статистика диспетчера
#[derive(Debug, Clone)]
pub struct DispatcherAdvancedStats {
    pub total_workers: usize,
    pub healthy_workers: usize,
    pub total_tasks_submitted: u64,
    pub total_tasks_processed: u64,
    pub successful_decryptions: u64,
    pub failed_decryptions: u64,
    pub work_steals: u64,
    pub avg_processing_time_ms: f64,
    pub p95_processing_time_ms: f64,
    pub p99_processing_time_ms: f64,
    pub current_batch_size: usize,
    pub batch_metrics: BatchMetrics,
    pub circuit_state: CircuitState,
    pub qos_quotas: (f64, f64, f64),
    pub qos_utilization: (f64, f64, f64),
    pub imbalance: f64,
    pub queue_backlog: usize,
    pub injector_backlog: usize,
    pub timestamp: Instant,
}

/// ⚡ Work-Stealing диспетчер с полной интеграцией
pub struct WorkStealingDispatcher {
    // 📦 Атомарные очереди для worker'ов
    pub worker_senders: Arc<Vec<Sender<WorkStealingTask>>>,
    pub worker_receivers: Arc<Vec<Receiver<WorkStealingTask>>>,
    pub worker_queues: Arc<DashMap<usize, usize>>,

    // 📦 Канал для инжектора (work stealing)
    injector_sender: Sender<WorkStealingTask>,
    injector_receiver: Receiver<WorkStealingTask>,
    injector_backlog: Arc<std::sync::atomic::AtomicUsize>,

    // 📦 Результаты
    results: Arc<DashMap<u64, WorkStealingResult>>,

    // 📊 Статистика
    stats: Arc<DashMap<String, u64>>,
    latency_histogram: Arc<DashMap<u64, u64>>,

    // 🎮 Управление
    is_running: Arc<std::sync::atomic::AtomicBool>,
    next_task_id: std::sync::atomic::AtomicU64,

    // 🔧 Обработка пакетов
    packet_processor: PhantomPacketProcessor,
    session_manager: Arc<PhantomSessionManager>,

    // 🔌 Интегрированные компоненты
    adaptive_batcher: Arc<AdaptiveBatcher>,
    qos_manager: Arc<QosManager>,
    circuit_breaker: Arc<CircuitBreaker>,

    // 🛡️ Защита от перегрузки
    backpressure_semaphore: Arc<Semaphore>,
    backpressure_threshold: usize,
}

impl WorkStealingDispatcher {
    /// 🚀 Создание нового диспетчера
    pub fn new(
        num_workers: usize,
        queue_capacity: usize,
        session_manager: Arc<PhantomSessionManager>,
        adaptive_batcher: Arc<AdaptiveBatcher>,
        qos_manager: Arc<QosManager>,
        circuit_breaker: Arc<CircuitBreaker>,
    ) -> Self {
        let mut worker_senders = Vec::with_capacity(num_workers);
        let mut worker_receivers = Vec::with_capacity(num_workers);
        let worker_queues = Arc::new(DashMap::with_capacity(num_workers));

        for i in 0..num_workers {
            let (tx, rx) = bounded(queue_capacity);
            worker_senders.push(tx);
            worker_receivers.push(rx);
            worker_queues.insert(i, 0);
        }

        let (injector_sender, injector_receiver) = bounded(queue_capacity * 4);
        let backpressure_threshold = (queue_capacity as f64 * 0.8) as usize;

        let dispatcher = Self {
            worker_senders: Arc::new(worker_senders),
            worker_receivers: Arc::new(worker_receivers),
            worker_queues,
            injector_sender,
            injector_receiver,
            injector_backlog: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            results: Arc::new(DashMap::with_capacity(10000)),
            stats: Arc::new(DashMap::new()),
            latency_histogram: Arc::new(DashMap::new()),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            next_task_id: std::sync::atomic::AtomicU64::new(1),
            packet_processor: PhantomPacketProcessor::new(),
            session_manager,
            adaptive_batcher,
            qos_manager,
            circuit_breaker,
            backpressure_semaphore: Arc::new(Semaphore::new(queue_capacity * num_workers)),
            backpressure_threshold,
        };

        dispatcher.start_workers();
        dispatcher.start_metrics_collector();
        dispatcher.start_queue_monitor();
        dispatcher.start_task_cleaner();

        info!("✅ WorkStealingDispatcher инициализирован с {} workers", num_workers);
        dispatcher
    }

    /// 👷 Запуск worker'ов
    fn start_workers(&self) {
        let num_workers = self.worker_senders.len();

        self.stats.insert("workers_started".to_string(), num_workers as u64);

        for worker_id in 0..num_workers {
            let worker_receiver = self.worker_receivers[worker_id].clone();
            let injector_receiver = self.injector_receiver.clone();
            let injector_sender = self.injector_sender.clone();
            let results = self.results.clone();
            let stats = self.stats.clone();
            let latency_histogram = self.latency_histogram.clone();
            let is_running = self.is_running.clone();
            let worker_queues = self.worker_queues.clone();

            let packet_processor = self.packet_processor.clone();
            let session_manager = self.session_manager.clone();

            let adaptive_batcher = self.adaptive_batcher.clone();
            let qos_manager = self.qos_manager.clone();
            let circuit_breaker = self.circuit_breaker.clone();

            tokio::spawn(async move {
                Self::worker_loop(
                    worker_id,
                    worker_receiver,
                    injector_receiver,
                    injector_sender,
                    results,
                    stats,
                    latency_histogram,
                    is_running,
                    worker_queues,
                    packet_processor,
                    session_manager,
                    adaptive_batcher,
                    qos_manager,
                    circuit_breaker,
                ).await;
            });
        }

        info!("✅ Запущено {} work-stealing workers", num_workers);
    }

    /// 🔄 Основной цикл worker'а
    #[allow(clippy::too_many_arguments)]
    async fn worker_loop(
        worker_id: usize,
        worker_receiver: Receiver<WorkStealingTask>,
        injector_receiver: Receiver<WorkStealingTask>,
        _injector_sender: Sender<WorkStealingTask>,
        results: Arc<DashMap<u64, WorkStealingResult>>,
        stats: Arc<DashMap<String, u64>>,
        latency_histogram: Arc<DashMap<u64, u64>>,
        is_running: Arc<std::sync::atomic::AtomicBool>,
        worker_queues: Arc<DashMap<usize, usize>>,
        packet_processor: PhantomPacketProcessor,
        session_manager: Arc<PhantomSessionManager>,
        adaptive_batcher: Arc<AdaptiveBatcher>,
        qos_manager: Arc<QosManager>,
        circuit_breaker: Arc<CircuitBreaker>,
    ) {
        debug!("👷 Worker #{} started", worker_id);

        let mut tasks_processed = 0;
        let mut successful_tasks = 0;
        let mut failed_tasks = 0;
        let mut batch_start_time = Instant::now();
        let mut batch_size = adaptive_batcher.get_batch_size().await;

        while is_running.load(std::sync::atomic::Ordering::Relaxed) {
            if !circuit_breaker.allow_request().await {
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            }

            if batch_start_time.elapsed() > Duration::from_millis(100) {
                batch_size = adaptive_batcher.get_batch_size().await;
            }

            tokio::select! {
                Ok(task) = worker_receiver.recv_async() => {
                    if let Some(mut q) = worker_queues.get_mut(&worker_id) {
                        *q = q.saturating_sub(1);
                    }

                    let permit = match qos_manager.acquire_permit(task.priority).await {
                        Ok(p) => p,
                        Err(e) => {
                            debug!("Worker #{} QoS failed: {}, task requeued", worker_id, e);
                            circuit_breaker.record_failure().await;
                            failed_tasks += 1;

                            if task.retry_count < 3 {
                                let mut retry_task = task.clone();
                                retry_task.retry_count += 1;
                                let _ = qos_manager.acquire_permit(Priority::Low).await;
                            }
                            continue;
                        }
                    };

                    let result = Self::process_task(
                        worker_id,
                        task,
                        &results,
                        &stats,
                        &latency_histogram,
                        &packet_processor,
                        &session_manager,
                    ).await;

                    tasks_processed += 1;
                    match result {
                        Ok(_) => {
                            successful_tasks += 1;
                            circuit_breaker.record_success().await;
                        }
                        Err(_) => {
                            failed_tasks += 1;
                            circuit_breaker.record_failure().await;
                        }
                    }

                    drop(permit);

                    if tasks_processed >= batch_size {
                        let elapsed = batch_start_time.elapsed();
                        let success_rate = if tasks_processed > 0 {
                            successful_tasks as f64 / tasks_processed as f64
                        } else {
                            1.0
                        };

                        stats.entry(format!("worker_{}_batches", worker_id))
                            .and_modify(|e| *e += 1)
                            .or_insert(1);

                        adaptive_batcher.record_batch_execution(
                            tasks_processed,
                            elapsed,
                            success_rate,
                            worker_queues.len(),
                        ).await;

                        tasks_processed = 0;
                        successful_tasks = 0;
                        failed_tasks = 0;
                        batch_start_time = Instant::now();
                    }
                }

                Ok(task) = injector_receiver.recv_async() => {
                    stats.entry("work_steals".to_string())
                        .and_modify(|e| *e += 1)
                        .or_insert(1);

                    let permit = match qos_manager.acquire_permit(task.priority).await {
                        Ok(p) => p,
                        Err(e) => {
                            debug!("Worker #{} (steal) QoS failed: {}", worker_id, e);
                            failed_tasks += 1;
                            continue;
                        }
                    };

                    let result = Self::process_task(
                        worker_id,
                        task,
                        &results,
                        &stats,
                        &latency_histogram,
                        &packet_processor,
                        &session_manager,
                    ).await;

                    tasks_processed += 1;
                    if result.is_ok() {
                        successful_tasks += 1;
                        circuit_breaker.record_success().await;
                    } else {
                        failed_tasks += 1;
                        circuit_breaker.record_failure().await;
                    }

                    drop(permit);
                }

                _ = tokio::time::sleep(Duration::from_micros(5)) => {
                    continue;
                }
            }
        }

        stats.insert(format!("worker_{}_final_tasks", worker_id), tasks_processed as u64);
        stats.insert(format!("worker_{}_final_success", worker_id), successful_tasks as u64);
        stats.insert(format!("worker_{}_final_failed", worker_id), failed_tasks as u64);

        debug!("👋 Worker #{} stopped", worker_id);
    }

    /// 🔐 Обработка задачи
    async fn process_task(
        worker_id: usize,
        task: WorkStealingTask,
        results: &Arc<DashMap<u64, WorkStealingResult>>,
        stats: &Arc<DashMap<String, u64>>,
        latency_histogram: &Arc<DashMap<u64, u64>>,
        packet_processor: &PhantomPacketProcessor,
        session_manager: &Arc<PhantomSessionManager>,
    ) -> Result<(), ()> {
        let start_time = Instant::now();

        if task.is_expired() {
            stats.entry("tasks_expired".to_string())
                .and_modify(|e| *e += 1)
                .or_insert(1);

            let result = WorkStealingResult {
                task_id: task.id,
                session_id: task.session_id,
                result: Err("Task expired".to_string()),
                processing_time: start_time.elapsed(),
                worker_id,
                destination_addr: task.source_addr,
                completed_at: Instant::now(),
            };
            results.insert(task.id, result);
            return Err(());
        }

        match session_manager.get_session(&task.session_id).await {
            Some(session) => {
                match packet_processor.process_incoming_vec(&task.data, &session) {
                    Ok((packet_type, decrypted_data)) => {
                        let mut result_data = Vec::with_capacity(decrypted_data.len() + 1);
                        result_data.push(packet_type);
                        result_data.extend_from_slice(&decrypted_data);

                        let processing_time = start_time.elapsed();
                        let processing_ms = processing_time.as_millis() as u64;

                        let result = WorkStealingResult {
                            task_id: task.id,
                            session_id: task.session_id,
                            result: Ok(result_data),
                            processing_time,
                            worker_id,
                            destination_addr: task.source_addr,
                            completed_at: Instant::now(),
                        };

                        results.insert(task.id, result);

                        stats.entry("total_tasks_processed".to_string())
                            .and_modify(|e| *e += 1)
                            .or_insert(1);

                        stats.entry(format!("worker_{}_tasks", worker_id))
                            .and_modify(|e| *e += 1)
                            .or_insert(1);

                        stats.entry("successful_decryptions".to_string())
                            .and_modify(|e| *e += 1)
                            .or_insert(1);

                        stats.entry("processing_time_ms_total".to_string())
                            .and_modify(|e| *e += processing_ms)
                            .or_insert(processing_ms);

                        stats.entry(format!("packet_type_{}", packet_type))
                            .and_modify(|e| *e += 1)
                            .or_insert(1);

                        latency_histogram.entry(processing_ms)
                            .and_modify(|e| *e += 1)
                            .or_insert(1);

                        Ok(())
                    }
                    Err(e) => {
                        error!("❌ Worker #{} decryption failed: {}", worker_id, e);

                        let result = WorkStealingResult {
                            task_id: task.id,
                            session_id: task.session_id,
                            result: Err(format!("Decryption failed: {}", e)),
                            processing_time: start_time.elapsed(),
                            worker_id,
                            destination_addr: task.source_addr,
                            completed_at: Instant::now(),
                        };

                        results.insert(task.id, result);

                        stats.entry("failed_decryptions".to_string())
                            .and_modify(|e| *e += 1)
                            .or_insert(1);

                        stats.entry(format!("worker_{}_errors", worker_id))
                            .and_modify(|e| *e += 1)
                            .or_insert(1);

                        Err(())
                    }
                }
            }
            None => {
                let error = format!("Session not found: {}", hex::encode(&task.session_id));
                debug!("Worker #{}: {}", worker_id, error);

                let result = WorkStealingResult {
                    task_id: task.id,
                    session_id: task.session_id,
                    result: Err(error),
                    processing_time: start_time.elapsed(),
                    worker_id,
                    destination_addr: task.source_addr,
                    completed_at: Instant::now(),
                };

                results.insert(task.id, result);

                stats.entry("session_not_found".to_string())
                    .and_modify(|e| *e += 1)
                    .or_insert(1);

                stats.entry(format!("worker_{}_session_errors", worker_id))
                    .and_modify(|e| *e += 1)
                    .or_insert(1);

                Err(())
            }
        }
    }

    /// 📊 Запуск сборщика метрик
    fn start_metrics_collector(&self) {
        let stats = self.stats.clone();
        let latency_histogram = self.latency_histogram.clone();
        let _adaptive_batcher = self.adaptive_batcher.clone();
        let is_running = self.is_running.clone();
        let worker_queues = self.worker_queues.clone();
        let results = self.results.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            stats.insert("metrics_collector_started".to_string(), 1);
            let mut collection_count = 0;

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;
                collection_count += 1;

                let mut latencies: Vec<u64> = latency_histogram.iter()
                    .flat_map(|e| vec![*e.key(); *e.value() as usize])
                    .collect();
                latencies.sort_unstable();

                let p50 = latencies.get(latencies.len() * 50 / 100).copied().unwrap_or(0);
                let p95 = latencies.get(latencies.len() * 95 / 100).copied().unwrap_or(0);
                let p99 = latencies.get(latencies.len() * 99 / 100).copied().unwrap_or(0);

                stats.insert("metrics_collection_count".to_string(), collection_count);
                stats.insert("current_p50_latency".to_string(), p50);
                stats.insert("current_p95_latency".to_string(), p95);
                stats.insert("current_p99_latency".to_string(), p99);

                let total_queue: usize = worker_queues.iter()
                    .map(|e| *e.value())
                    .sum();
                stats.insert("current_total_queue".to_string(), total_queue as u64);

                let active_results = results.len() as u64;
                stats.insert("active_results".to_string(), active_results);
            }

            stats.insert("metrics_collector_stopped".to_string(), 1);
            debug!("📊 Metrics collector stopped");
        });
    }

    /// 📊 Запуск монитора очередей
    fn start_queue_monitor(&self) {
        let worker_senders = self.worker_senders.clone();
        let worker_queues = self.worker_queues.clone();
        let injector_backlog = self.injector_backlog.clone();
        let _backpressure_semaphore = self.backpressure_semaphore.clone();
        let is_running = self.is_running.clone();
        let stats = self.stats.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            let mut monitor_count = 0;
            let mut peak_queue = 0;
            let mut peak_injector = 0;

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;
                monitor_count += 1;

                let mut total_queue = 0;
                let mut overloaded_workers = 0;

                for (i, sender) in worker_senders.iter().enumerate() {
                    let len = sender.len();
                    worker_queues.insert(i, len);
                    total_queue += len;

                    if len > 500 {
                        overloaded_workers += 1;
                    }
                }

                let injector_len = worker_senders[0].len();
                injector_backlog.store(injector_len, std::sync::atomic::Ordering::Relaxed);

                peak_queue = peak_queue.max(total_queue);
                peak_injector = peak_injector.max(injector_len);

                if monitor_count % 100 == 0 {
                    stats.insert("peak_queue_size".to_string(), peak_queue as u64);
                    stats.insert("peak_injector_size".to_string(), peak_injector as u64);
                    stats.insert("overloaded_workers_count".to_string(), overloaded_workers as u64);
                    stats.insert("queue_monitor_checks".to_string(), monitor_count);
                }

                if total_queue > worker_senders.len() * 500 {
                    stats.entry("high_queue_warnings".to_string())
                        .and_modify(|e| *e += 1)
                        .or_insert(1);
                }
            }

            stats.insert("queue_monitor_stopped".to_string(), 1);
            debug!("📊 Queue monitor stopped");
        });
    }

    /// 🧹 Запуск очистки устаревших задач
    fn start_task_cleaner(&self) {
        let results = self.results.clone();
        let is_running = self.is_running.clone();
        let stats = self.stats.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            let mut cleanup_count = 0;
            let mut total_cleaned = 0;

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;
                cleanup_count += 1;

                let now = Instant::now();
                let mut to_remove = Vec::new();

                for entry in results.iter() {
                    if now.duration_since(entry.completed_at) > Duration::from_secs(300) {
                        to_remove.push(*entry.key());
                    }
                }

                for key in to_remove.clone() {
                    results.remove(&key);
                }

                let cleaned = to_remove.len();
                total_cleaned += cleaned;

                stats.entry("total_cleaned_tasks".to_string())
                    .and_modify(|e| *e += cleaned as u64)
                    .or_insert(cleaned as u64);

                stats.entry("cleanup_operations".to_string())
                    .and_modify(|e| *e += 1)
                    .or_insert(1);

                if !to_remove.is_empty() {
                    debug!("🧹 Cleanup #{}: removed {} old tasks", cleanup_count, cleaned);
                }
            }

            stats.insert("final_cleanup_count".to_string(), cleanup_count);
            stats.insert("final_total_cleaned".to_string(), total_cleaned as u64);
            debug!("🧹 Task cleaner stopped");
        });
    }

    /// 📤 Отправка задачи
    pub async fn submit_task(&self, mut task: WorkStealingTask) -> Result<u64, BatchError> {
        if !self.circuit_breaker.allow_request().await {
            self.circuit_breaker.record_failure().await;
            return Err(BatchError::ProcessingError("Circuit breaker is open".to_string()));
        }

        let task_id = self.next_task_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        task.id = task_id;

        if task.deadline.is_none() {
            task.deadline = Some(Instant::now() + Duration::from_secs(30));
        }

        let total_backlog = self.worker_queues.iter()
            .map(|e| *e.value())
            .sum::<usize>() + self.injector_backlog.load(std::sync::atomic::Ordering::Relaxed);

        if total_backlog > self.backpressure_threshold * self.worker_senders.len() {
            self.circuit_breaker.record_failure().await;
            return Err(BatchError::Backpressure);
        }

        if let Some(target_worker_id) = task.worker_id {
            if target_worker_id < self.worker_senders.len() {
                if let Ok(()) = self.worker_senders[target_worker_id].try_send(task.clone()) {
                    if let Some(mut q) = self.worker_queues.get_mut(&target_worker_id) {
                        *q = q.saturating_add(1);
                    }

                    self.stats.entry("tasks_submitted".to_string())
                        .and_modify(|e| *e += 1)
                        .or_insert(1);

                    self.circuit_breaker.record_success().await;
                    return Ok(task_id);
                }
            }
        }

        let worker_idx = task_id as usize % self.worker_senders.len();

        match self.worker_senders[worker_idx].try_send(task.clone()) {
            Ok(()) => {
                if let Some(mut q) = self.worker_queues.get_mut(&worker_idx) {
                    *q = q.saturating_add(1);
                }

                self.stats.entry("tasks_submitted".to_string())
                    .and_modify(|e| *e += 1)
                    .or_insert(1);

                self.circuit_breaker.record_success().await;
                Ok(task_id)
            }
            Err(_) => {
                match self.injector_sender.try_send(task) {
                    Ok(()) => {
                        self.injector_backlog.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                        self.stats.entry("tasks_submitted".to_string())
                            .and_modify(|e| *e += 1)
                            .or_insert(1);

                        self.circuit_breaker.record_success().await;
                        Ok(task_id)
                    }
                    Err(_) => {
                        self.circuit_breaker.record_failure().await;
                        Err(BatchError::Backpressure)
                    }
                }
            }
        }
    }

    /// 📥 Получение результата
    pub fn get_result(&self, task_id: u64) -> Option<WorkStealingResult> {
        self.results.get(&task_id).map(|r| r.clone())
    }

    /// 📊 Получение базовой статистики
    pub fn get_stats(&self) -> std::collections::HashMap<String, u64> {
        let mut stats_map = std::collections::HashMap::new();

        for entry in self.stats.iter() {
            stats_map.insert(entry.key().clone(), *entry.value());
        }

        let total_queue: usize = self.worker_queues.iter()
            .map(|e| *e.value())
            .sum();
        stats_map.insert("current_queue_size".to_string(), total_queue as u64);

        let injector_backlog = self.injector_backlog.load(std::sync::atomic::Ordering::Relaxed);
        stats_map.insert("injector_backlog".to_string(), injector_backlog as u64);

        stats_map
    }

    /// 📈 Получение расширенной статистики
    pub async fn get_advanced_stats(&self) -> DispatcherAdvancedStats {
        let stats = self.get_stats();

        let total_processed = stats.get("total_tasks_processed").copied().unwrap_or(0);
        let total_submitted = stats.get("tasks_submitted").copied().unwrap_or(0);
        let steals = stats.get("work_steals").copied().unwrap_or(0);
        let successful = stats.get("successful_decryptions").copied().unwrap_or(0);
        let failed = stats.get("failed_decryptions").copied().unwrap_or(0);

        let processing_time_total = stats.get("processing_time_ms_total").copied().unwrap_or(0);
        let avg_processing_time_ms = if total_processed > 0 {
            processing_time_total as f64 / total_processed as f64
        } else { 0.0 };

        let mut latencies: Vec<u64> = self.latency_histogram.iter()
            .flat_map(|e| vec![*e.key(); *e.value() as usize])
            .collect();
        latencies.sort_unstable();

        let p95 = latencies.get(latencies.len() * 95 / 100).copied().unwrap_or(0) as f64;
        let p99 = latencies.get(latencies.len() * 99 / 100).copied().unwrap_or(0) as f64;

        let batch_metrics = self.adaptive_batcher.get_metrics().await;
        let current_batch_size = self.adaptive_batcher.get_batch_size().await;

        let circuit_state = self.circuit_breaker.get_state().await;

        let qos_quotas = self.qos_manager.get_quotas().await;
        let qos_utilization = self.qos_manager.get_utilization().await;

        let mut healthy_workers = 0;
        for i in 0..self.worker_senders.len() {
            let processed = stats.get(&format!("worker_{}_tasks", i)).copied().unwrap_or(0);
            if processed > 0 {
                healthy_workers += 1;
            }
        }

        let imbalance = self.calculate_imbalance().await;

        let queue_backlog: usize = self.worker_queues.iter()
            .map(|e| *e.value())
            .sum();
        let injector_backlog = self.injector_backlog.load(std::sync::atomic::Ordering::Relaxed);

        DispatcherAdvancedStats {
            total_workers: self.worker_senders.len(),
            healthy_workers,
            total_tasks_submitted: total_submitted,
            total_tasks_processed: total_processed,
            successful_decryptions: successful,
            failed_decryptions: failed,
            work_steals: steals,
            avg_processing_time_ms,
            p95_processing_time_ms: p95,
            p99_processing_time_ms: p99,
            current_batch_size,
            batch_metrics,
            circuit_state,
            qos_quotas,
            qos_utilization,
            imbalance,
            queue_backlog,
            injector_backlog,
            timestamp: Instant::now(),
        }
    }

    /// ⚖️ Расчет дисбаланса нагрузки
    async fn calculate_imbalance(&self) -> f64 {
        let mut worker_loads = Vec::new();
        let stats = self.get_stats();

        for i in 0..self.worker_senders.len() {
            let processed = stats.get(&format!("worker_{}_tasks", i)).copied().unwrap_or(0);
            worker_loads.push(processed as f64);
        }

        if worker_loads.is_empty() {
            return 0.0;
        }

        let avg = worker_loads.iter().sum::<f64>() / worker_loads.len() as f64;
        if avg == 0.0 {
            return 0.0;
        }

        let variance = worker_loads.iter()
            .map(|&x| (x - avg).powi(2))
            .sum::<f64>() / worker_loads.len() as f64;

        (variance.sqrt() / avg).min(1.0)
    }

    /// 🛑 Остановка
    pub async fn shutdown(&self) {
        info!("🛑 Shutting down work-stealing dispatcher...");
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);

        self.worker_queues.clear();
        self.results.clear();
        self.stats.clear();
        self.latency_histogram.clear();

        info!("✅ Work-stealing dispatcher stopped");
    }

    /// 📦 Получение AdaptiveBatcher
    pub fn get_adaptive_batcher(&self) -> Arc<AdaptiveBatcher> {
        self.adaptive_batcher.clone()
    }

    /// 📦 Получение QoS Manager
    pub fn get_qos_manager(&self) -> Arc<QosManager> {
        self.qos_manager.clone()
    }

    /// 📦 Получение Circuit Breaker
    pub fn get_circuit_breaker(&self) -> Arc<CircuitBreaker> {
        self.circuit_breaker.clone()
    }
}

impl Drop for WorkStealingDispatcher {
    fn drop(&mut self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Clone for WorkStealingDispatcher {
    fn clone(&self) -> Self {
        Self {
            worker_senders: self.worker_senders.clone(),
            worker_receivers: self.worker_receivers.clone(),
            worker_queues: self.worker_queues.clone(),
            injector_sender: self.injector_sender.clone(),
            injector_receiver: self.injector_receiver.clone(),
            injector_backlog: self.injector_backlog.clone(),
            results: self.results.clone(),
            stats: self.stats.clone(),
            latency_histogram: self.latency_histogram.clone(),
            is_running: self.is_running.clone(),
            next_task_id: std::sync::atomic::AtomicU64::new(
                self.next_task_id.load(std::sync::atomic::Ordering::Relaxed)
            ),
            packet_processor: self.packet_processor.clone(),
            session_manager: self.session_manager.clone(),
            adaptive_batcher: self.adaptive_batcher.clone(),
            qos_manager: self.qos_manager.clone(),
            circuit_breaker: self.circuit_breaker.clone(),
            backpressure_semaphore: self.backpressure_semaphore.clone(),
            backpressure_threshold: self.backpressure_threshold,
        }
    }
}