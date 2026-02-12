use std::sync::Arc;
use std::time::{Instant, Duration};
use dashmap::DashMap;
use tracing::{info, debug, error};
use bytes::Bytes;
use flume::{Sender, Receiver, bounded};
use tokio::sync::{RwLock, Semaphore};

use crate::core::protocol::batch_system::types::error::BatchError;
use crate::core::protocol::batch_system::types::priority::Priority;
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::batch_system::adaptive_batcher::AdaptiveBatcher;
use crate::core::protocol::batch_system::qos_manager::QosManager;
use crate::core::protocol::batch_system::circuit_breaker::{CircuitBreaker, CircuitState};

/// Модель случайного кражи задач
#[derive(Debug, Clone)]
pub struct WorkStealingModel {
    /// Количество воркеров
    pub m: usize,
    /// Вероятность кражи
    pub p_steal: f64,
    /// Интенсивность кражи
    pub lambda_steal: f64,
    /// Скорость обработки
    pub mu: f64,
    /// Средний размер батча
    pub avg_batch_size: f64,
    /// Текущая загрузка
    pub rho: f64,
    /// Дисбаланс (коэффициент вариации)
    pub imbalance: f64,
    /// Стоимость простоя
    pub cost_idle: f64,
    /// Стоимость кражи
    pub cost_steal: f64,
}

impl WorkStealingModel {
    pub fn new(m: usize) -> Self {
        Self {
            m,
            p_steal: 0.1,
            lambda_steal: 0.0,
            mu: 1000.0,
            avg_batch_size: 256.0,
            rho: 0.0,
            imbalance: 0.0,
            cost_idle: 0.5,
            cost_steal: 0.2,
        }
    }

    /// Вероятность успешной кражи
    pub fn steal_success_probability(&self, queue_len: usize) -> f64 {
        if queue_len == 0 {
            0.0
        } else {
            1.0 - 1.0 / (queue_len as f64 + 1.0)
        }
    }

    /// Интенсивность кражи: λ_steal = p_steal·(M-1)/M·r
    pub fn compute_steal_intensity(&self, check_rate: f64) -> f64 {
        self.p_steal * (self.m - 1) as f64 / self.m as f64 * check_rate
    }

    /// Коэффициент дисбаланса (нормированный коэффициент вариации)
    pub fn compute_imbalance(loads: &[f64]) -> f64 {
        if loads.is_empty() {
            return 0.0;
        }

        let mean = loads.iter().sum::<f64>() / loads.len() as f64;
        if mean < 1e-6 {
            return 0.0;
        }

        let variance = loads.iter()
            .map(|&x| (x - mean).powi(2))
            .sum::<f64>() / loads.len() as f64;

        (variance.sqrt() / mean).min(1.0)
    }

    /// Оптимальная политика кражи (уравнение Беллмана)
    pub fn optimal_steal_policy(&self, queue_len: usize, _cost_idle: f64, cost_steal: f64) -> bool {
        if queue_len == 0 {
            return false;
        }

        let p_success = self.steal_success_probability(queue_len);
        let benefit = p_success * (queue_len as f64) * 0.1; // упрощённая функция ценности

        benefit > cost_steal
    }
}

/// Распределение задач по воркерам (Power of Two Choices)
#[derive(Debug, Clone)]
pub struct LoadBalancer {
    /// История загрузки воркеров
    pub worker_loads: Vec<f64>,
    /// Скользящее среднее
    pub ema_alpha: f64,
    /// Предсказанная нагрузка
    pub predicted_loads: Vec<f64>,
}

impl LoadBalancer {
    pub fn new(num_workers: usize) -> Self {
        Self {
            worker_loads: vec![0.0; num_workers],
            ema_alpha: 0.3,
            predicted_loads: vec![0.0; num_workers],
        }
    }

    /// Выбор воркера методом "Power of Two Choices"
    pub fn select_worker_power_of_two(&self) -> (usize, usize) {
        let idx1 = rand::random::<usize>() % self.worker_loads.len();
        let idx2 = rand::random::<usize>() % self.worker_loads.len();

        if self.worker_loads[idx1] < self.worker_loads[idx2] {
            (idx1, idx2)
        } else {
            (idx2, idx1)
        }
    }

    /// Экспоненциально-взвешенное скользящее среднее
    pub fn update_ema(&mut self, worker_id: usize, current_load: f64) {
        self.worker_loads[worker_id] = self.ema_alpha * current_load +
            (1.0 - self.ema_alpha) * self.worker_loads[worker_id];
    }
}

#[derive(Debug, Clone)]
pub struct WorkerQueueModel {
    /// Интенсивность поступления
    pub lambda: f64,
    /// Интенсивность обслуживания
    pub mu: f64,
    /// Количество воркеров
    pub m: usize,
    /// Коэффициент загрузки
    pub rho: f64,
}

impl WorkerQueueModel {
    pub fn new(m: usize, mu: f64) -> Self {
        Self {
            lambda: 0.0,
            mu,
            m,
            rho: 0.0,
        }
    }

    /// Вероятность состояния n в M/M/m очереди
    pub fn mm2_probability(&self, n: usize) -> f64 {
        if self.rho >= 1.0 {
            return 0.0;
        }

        let p0 = 1.0 / ((0..self.m).map(|k| (self.m as f64 * self.rho).powi(k as i32) / self.factorial(k))
            .sum::<f64>() +
            (self.m as f64 * self.rho).powi(self.m as i32) /
                (self.factorial(self.m) * (1.0 - self.rho)));

        if n < self.m {
            p0 * (self.m as f64 * self.rho).powi(n as i32) / self.factorial(n)
        } else {
            p0 * (self.m as f64).powi(self.m as i32) * self.rho.powi(n as i32) /
                self.factorial(self.m)
        }
    }

    /// Средняя длина очереди в M/M/m
    pub fn average_queue_length(&self) -> f64 {
        if self.rho >= 1.0 {
            return f64::INFINITY;
        }

        let p0 = self.mm2_probability(0);
        let term = p0 * (self.m as f64 * self.rho).powi(self.m as i32) * self.rho /
            (self.factorial(self.m) * (1.0 - self.rho).powi(2));

        term
    }

    /// Среднее время ожидания в очереди
    pub fn average_waiting_time(&self) -> f64 {
        if self.rho >= 1.0 {
            return f64::INFINITY;
        }

        self.average_queue_length() / self.lambda
    }

    fn factorial(&self, n: usize) -> f64 {
        (1..=n).fold(1.0, |acc, x| acc * x as f64)
    }
}

/// Оптимизация количества воркеров на основе теории массового обслуживания
pub fn optimal_worker_count(lambda: f64, mu: f64, target_wait_time: f64) -> usize {
    if lambda <= 0.0 || mu <= 0.0 {
        return 4;
    }

    let mut m = 1;
    loop {
        let rho = lambda / (m as f64 * mu);
        if rho >= 1.0 {
            m += 1;
            continue;
        }

        let model = WorkerQueueModel::new(m, mu);
        let wait_time = model.average_waiting_time();

        if wait_time <= target_wait_time || m > 64 {
            break;
        }

        m += 1;
    }

    m
}

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
    pub size_bytes: usize,
    pub estimated_processing_time: f64,
}

impl WorkStealingTask {
    pub fn is_expired(&self) -> bool {
        self.deadline.map_or(false, |d| Instant::now() > d)
    }

    /// Оценка времени обработки на основе размера
    pub fn estimate_processing_time(&mut self) {
        self.estimated_processing_time = self.data.len() as f64 * 0.001 + 0.1; // 0.001 ms/byte + 0.1 ms
    }
}

#[derive(Debug, Clone)]
pub struct WorkStealingResult {
    pub task_id: u64,
    pub session_id: Vec<u8>,
    pub result: Result<Vec<u8>, String>,
    pub processing_time: Duration,
    pub worker_id: usize,
    pub destination_addr: std::net::SocketAddr,
    pub completed_at: Instant,
    pub was_stolen: bool,
}

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
    pub batch_metrics: Option<()>, // Заглушка для метрик батчера
    pub circuit_state: CircuitState,
    pub qos_quotas: (f64, f64, f64),
    pub qos_utilization: (f64, f64, f64),
    pub imbalance: f64,
    pub queue_backlog: usize,
    pub injector_backlog: usize,
    pub steal_probability: f64,
    pub optimal_workers: usize,
    pub predicted_lambda: f64,
    pub timestamp: Instant,
}

pub struct WorkStealingDispatcher {
    pub worker_senders: Arc<Vec<Sender<WorkStealingTask>>>,
    pub worker_receivers: Arc<Vec<Receiver<WorkStealingTask>>>,
    pub worker_queues: Arc<DashMap<usize, usize>>,
    pub worker_loads: Arc<DashMap<usize, f64>>,
    pub worker_throughputs: Arc<DashMap<usize, f64>>,
    injector_sender: Sender<WorkStealingTask>,
    injector_receiver: Receiver<WorkStealingTask>,
    injector_backlog: Arc<std::sync::atomic::AtomicUsize>,
    results: Arc<DashMap<u64, WorkStealingResult>>,
    pub stealing_model: Arc<RwLock<WorkStealingModel>>,
    pub load_balancer: Arc<RwLock<LoadBalancer>>,
    pub queue_models: Arc<DashMap<usize, WorkerQueueModel>>,
    stats: Arc<DashMap<String, u64>>,
    latency_histogram: Arc<DashMap<u64, u64>>,
    worker_latency_history: Arc<DashMap<usize, Vec<f64>>>,
    is_running: Arc<std::sync::atomic::AtomicBool>,
    next_task_id: std::sync::atomic::AtomicU64,
    packet_processor: PhantomPacketProcessor,
    session_manager: Arc<PhantomSessionManager>,
    adaptive_batcher: Arc<AdaptiveBatcher>,
    qos_manager: Arc<QosManager>,
    circuit_breaker: Arc<CircuitBreaker>,
    backpressure_semaphore: Arc<Semaphore>,
    backpressure_threshold: usize,
    check_rate: f64,
    cost_idle: f64,
    cost_steal: f64,
}

impl WorkStealingDispatcher {
    pub fn new(
        num_workers: usize,
        queue_capacity: usize,
        session_manager: Arc<PhantomSessionManager>,
        adaptive_batcher: Arc<AdaptiveBatcher>,
        qos_manager: Arc<QosManager>,
        circuit_breaker: Arc<CircuitBreaker>,
    ) -> Self {
        info!("🚀 Initializing Mathematical WorkStealingDispatcher v2.0");

        let mut worker_senders = Vec::with_capacity(num_workers);
        let mut worker_receivers = Vec::with_capacity(num_workers);
        let worker_queues = Arc::new(DashMap::with_capacity(num_workers));
        let worker_loads = Arc::new(DashMap::with_capacity(num_workers));
        let worker_throughputs = Arc::new(DashMap::with_capacity(num_workers));

        for i in 0..num_workers {
            let (tx, rx) = bounded(queue_capacity);
            worker_senders.push(tx);
            worker_receivers.push(rx);
            worker_queues.insert(i, 0);
            worker_loads.insert(i, 0.0);
            worker_throughputs.insert(i, 0.0);
        }

        let (injector_sender, injector_receiver) = bounded(queue_capacity * 4);

        let stealing_model = Arc::new(RwLock::new(WorkStealingModel::new(num_workers)));
        let load_balancer = Arc::new(RwLock::new(LoadBalancer::new(num_workers)));
        let queue_models = Arc::new(DashMap::with_capacity(num_workers));

        for i in 0..num_workers {
            queue_models.insert(i, WorkerQueueModel::new(1, 1000.0));
        }

        let backpressure_threshold = (queue_capacity as f64 * 0.8) as usize;

        let dispatcher = Self {
            worker_senders: Arc::new(worker_senders),
            worker_receivers: Arc::new(worker_receivers),
            worker_queues,
            worker_loads,
            worker_throughputs,
            injector_sender,
            injector_receiver,
            injector_backlog: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            results: Arc::new(DashMap::with_capacity(10000)),
            stealing_model,
            load_balancer,
            queue_models,
            stats: Arc::new(DashMap::new()),
            latency_histogram: Arc::new(DashMap::new()),
            worker_latency_history: Arc::new(DashMap::new()),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            next_task_id: std::sync::atomic::AtomicU64::new(1),
            packet_processor: PhantomPacketProcessor::new(),
            session_manager,
            adaptive_batcher,
            qos_manager,
            circuit_breaker,
            backpressure_semaphore: Arc::new(Semaphore::new(queue_capacity * num_workers)),
            backpressure_threshold,
            check_rate: 100.0, // 100 checks/sec
            cost_idle: 0.5,
            cost_steal: 0.2,
        };

        dispatcher.start_workers();
        dispatcher.start_stealing_optimizer();
        dispatcher.start_load_monitor();
        dispatcher.start_metrics_collector();
        dispatcher.start_task_cleaner();

        info!("✅ WorkStealingDispatcher initialized with {} workers", num_workers);

        dispatcher
    }

    pub fn start_workers(&self) {
        let num_workers = self.worker_senders.len();
        self.stats.insert("workers_started".to_string(), num_workers as u64);

        for worker_id in 0..num_workers {
            let worker_receiver = self.worker_receivers[worker_id].clone();
            let injector_receiver = self.injector_receiver.clone();
            let results = self.results.clone();
            let stats = self.stats.clone();
            let latency_histogram = self.latency_histogram.clone();
            let worker_latency_history = self.worker_latency_history.clone();
            let is_running = self.is_running.clone();
            let worker_queues = self.worker_queues.clone();
            let worker_loads = self.worker_loads.clone();
            let worker_throughputs = self.worker_throughputs.clone();
            let stealing_model = self.stealing_model.clone();
            let cost_steal = self.cost_steal;
            let cost_idle = self.cost_idle;

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
                    results,
                    stats,
                    latency_histogram,
                    worker_latency_history,
                    is_running,
                    worker_queues,
                    worker_loads,
                    worker_throughputs,
                    stealing_model,
                    packet_processor,
                    session_manager,
                    adaptive_batcher,
                    qos_manager,
                    circuit_breaker,
                    cost_idle,
                    cost_steal,
                ).await;
            });
        }

        info!("✅ Started {} mathematical work-stealing workers", num_workers);
    }

    #[allow(clippy::too_many_arguments)]
    async fn worker_loop(
        worker_id: usize,
        worker_receiver: Receiver<WorkStealingTask>,
        injector_receiver: Receiver<WorkStealingTask>,
        results: Arc<DashMap<u64, WorkStealingResult>>,
        stats: Arc<DashMap<String, u64>>,
        latency_histogram: Arc<DashMap<u64, u64>>,
        _worker_latency_history: Arc<DashMap<usize, Vec<f64>>>,
        is_running: Arc<std::sync::atomic::AtomicBool>,
        worker_queues: Arc<DashMap<usize, usize>>,
        worker_loads: Arc<DashMap<usize, f64>>,
        worker_throughputs: Arc<DashMap<usize, f64>>,
        stealing_model: Arc<RwLock<WorkStealingModel>>,
        packet_processor: PhantomPacketProcessor,
        session_manager: Arc<PhantomSessionManager>,
        adaptive_batcher: Arc<AdaptiveBatcher>,
        qos_manager: Arc<QosManager>,
        circuit_breaker: Arc<CircuitBreaker>,
        cost_idle: f64,
        cost_steal: f64,
    ) {
        debug!("👷 Mathematical worker #{} started", worker_id);

        let mut tasks_processed = 0;
        let mut successful_tasks = 0;
        let mut failed_tasks = 0;
        let mut stolen_tasks = 0;
        let mut processing_times = Vec::with_capacity(100);
        let mut batch_start_time = Instant::now();
        let mut batch_size = adaptive_batcher.get_batch_size().await;
        let mut last_steal_check = Instant::now();

        while is_running.load(std::sync::atomic::Ordering::Relaxed) {
            if !circuit_breaker.allow_request().await {
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            }

            if batch_start_time.elapsed() > Duration::from_millis(100) {
                batch_size = adaptive_batcher.get_batch_size().await;
                batch_start_time = Instant::now();
            }

            let should_steal = if last_steal_check.elapsed() > Duration::from_millis(10) {
                last_steal_check = Instant::now();

                let queue_len = worker_queues
                    .get(&worker_id)
                    .map(|q| *q.value())
                    .unwrap_or(0);

                let model = stealing_model.read().await;
                model.optimal_steal_policy(queue_len, cost_idle, cost_steal)
            } else {
                false
            };

            tokio::select! {
                // Получение задачи из своей очереди
                Ok(task) = worker_receiver.recv_async() => {
                    // Обновление длины очереди
                    if let Some(mut q) = worker_queues.get_mut(&worker_id) {
                        *q = q.saturating_sub(1);
                    }

                    // Получение QoS разрешения
                    let permit = match qos_manager.acquire_permit(task.priority).await {
                        Ok(p) => p,
                        Err(e) => {
                            debug!("Worker #{} QoS failed: {}, task requeued", worker_id, e);
                            circuit_breaker.record_failure().await;
                            failed_tasks += 1;

                            if task.retry_count < 3 {
                                let mut retry_task = task;
                                retry_task.retry_count += 1;
                                let _ = qos_manager.acquire_permit(Priority::Low).await;
                            }
                            continue;
                        }
                    };

                    // Обработка задачи
                    let start_time = Instant::now();
                    let result = Self::process_task(
                        worker_id,
                        task,
                        &results,
                        &stats,
                        &latency_histogram,
                        &packet_processor,
                        &session_manager,
                        false, // not stolen
                    ).await;

                    let processing_time = start_time.elapsed();
                    let processing_ms = processing_time.as_millis() as f64;

                    // Обновление статистики
                    tasks_processed += 1;
                    processing_times.push(processing_ms);
                    if processing_times.len() > 100 {
                        processing_times.remove(0);
                    }

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

                    // Обновление throughput воркера
                    if let Some(mut tp) = worker_throughputs.get_mut(&worker_id) {
                        *tp = 1000.0 / processing_ms; // ops/sec
                    }

                    drop(permit);

                    // Запись батча
                    if tasks_processed >= batch_size {
                        let elapsed = batch_start_time.elapsed();
                        let success_rate = if tasks_processed > 0 {
                            successful_tasks as f64 / tasks_processed as f64
                        } else {
                            1.0
                        };

                        let avg_processing = processing_times.iter().sum::<f64>() /
                                           processing_times.len().max(1) as f64;

                        // Обновление загрузки воркера (EMA)
                        if let Some(mut load) = worker_loads.get_mut(&worker_id) {
                            *load = 0.3 * avg_processing + 0.7 * *load;
                        }

                        stats.entry(format!("worker_{}_batches", worker_id))
                            .and_modify(|e| *e += 1)
                            .or_insert(1);

                        stats.entry(format!("worker_{}_stolen", worker_id))
                            .and_modify(|e| *e += stolen_tasks as u64)
                            .or_insert(stolen_tasks as u64);

                        adaptive_batcher.record_batch_execution(
                            tasks_processed,
                            elapsed,
                            success_rate,
                            worker_queues.len(),
                        ).await;

                        tasks_processed = 0;
                        successful_tasks = 0;
                        failed_tasks = 0;
                        stolen_tasks = 0;
                        batch_start_time = Instant::now();
                    }
                }

                // Кража задачи из injector
                Ok(task) = injector_receiver.recv_async(), if should_steal => {
                    stats.entry("work_steals".to_string())
                        .and_modify(|e| *e += 1)
                        .or_insert(1);

                    stolen_tasks += 1;

                    let permit = match qos_manager.acquire_permit(task.priority).await {
                        Ok(p) => p,
                        Err(e) => {
                            debug!("Worker #{} (steal) QoS failed: {}", worker_id, e);
                            failed_tasks += 1;
                            continue;
                        }
                    };

                    let start_time = Instant::now();
                    let result = Self::process_task(
                        worker_id,
                        task,
                        &results,
                        &stats,
                        &latency_histogram,
                        &packet_processor,
                        &session_manager,
                        true, // stolen
                    ).await;

                    let processing_time = start_time.elapsed();
                    let processing_ms = processing_time.as_millis() as f64;

                    tasks_processed += 1;
                    processing_times.push(processing_ms);

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

        // Финальная статистика
        stats.insert(format!("worker_{}_final_tasks", worker_id), tasks_processed as u64);
        stats.insert(format!("worker_{}_final_success", worker_id), successful_tasks as u64);
        stats.insert(format!("worker_{}_final_failed", worker_id), failed_tasks as u64);
        stats.insert(format!("worker_{}_final_stolen", worker_id), stolen_tasks as u64);

        debug!("👋 Mathematical worker #{} stopped", worker_id);
    }

    async fn process_task(
        worker_id: usize,
        task: WorkStealingTask,
        results: &Arc<DashMap<u64, WorkStealingResult>>,
        stats: &Arc<DashMap<String, u64>>,
        latency_histogram: &Arc<DashMap<u64, u64>>,
        packet_processor: &PhantomPacketProcessor,
        session_manager: &Arc<PhantomSessionManager>,
        was_stolen: bool,
    ) -> Result<(), ()> {
        let start_time = Instant::now();

        // Проверка на истечение срока
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
                was_stolen,
            };
            results.insert(task.id, result);
            return Err(());
        }

        // Поиск сессии
        match session_manager.get_session(&task.session_id).await {
            Some(session) => {
                // Обработка пакета
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
                            was_stolen,
                        };

                        results.insert(task.id, result);

                        // Обновление статистики
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
                            was_stolen,
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
                    was_stolen,
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

    pub fn start_stealing_optimizer(&self) {
        let stealing_model = self.stealing_model.clone();
        let worker_queues = self.worker_queues.clone();
        let worker_senders = self.worker_senders.clone();
        let injector_backlog = self.injector_backlog.clone();
        let is_running = self.is_running.clone();
        let stats = self.stats.clone();
        let check_rate = self.check_rate;
        let cost_idle = self.cost_idle;
        let cost_steal = self.cost_steal;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let mut total_queue = 0;
                let mut worker_loads = Vec::new();

                for i in 0..worker_senders.len() {
                    if let Some(q) = worker_queues.get(&i) {
                        let len = *q.value();
                        total_queue += len;
                        worker_loads.push(len as f64);
                    }
                }

                let _injector_len = injector_backlog.load(std::sync::atomic::Ordering::Relaxed);

                let mut model = stealing_model.write().await;
                model.m = worker_senders.len();
                model.p_steal = 0.1 + (total_queue as f64 / 10000.0).min(0.3);
                model.lambda_steal = model.compute_steal_intensity(check_rate);
                model.imbalance = WorkStealingModel::compute_imbalance(&worker_loads);
                model.cost_idle = cost_idle;
                model.cost_steal = cost_steal;

                if model.imbalance > 0.5 {
                    model.p_steal = (model.p_steal + 0.05).min(0.5);
                } else if model.imbalance < 0.2 {
                    model.p_steal = (model.p_steal - 0.01).max(0.05);
                }

                stats.insert("steal_probability".to_string(), (model.p_steal * 1000.0) as u64);
                stats.insert("steal_lambda".to_string(), (model.lambda_steal * 100.0) as u64);
                stats.insert("steal_imbalance".to_string(), (model.imbalance * 1000.0) as u64);
            }
        });
    }

    pub fn start_load_monitor(&self) {
        let worker_senders = self.worker_senders.clone();
        let worker_loads = self.worker_loads.clone();
        let worker_throughputs = self.worker_throughputs.clone();
        let worker_queues = self.worker_queues.clone();
        let queue_models = self.queue_models.clone();
        let load_balancer = self.load_balancer.clone();
        let stats = self.stats.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(1000));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let mut total_load = 0.0;
                let mut total_throughput = 0.0;
                let mut loads = Vec::new();

                for i in 0..worker_senders.len() {
                    if let Some(load) = worker_loads.get(&i) {
                        total_load += *load.value();
                        loads.push(*load.value());
                    }

                    if let Some(tp) = worker_throughputs.get(&i) {
                        total_throughput += *tp.value();
                    }

                    if let Some(q) = worker_queues.get(&i) {
                        let queue_len = *q.value();

                        // Обновление модели очереди
                        if let Some(mut model) = queue_models.get_mut(&i) {
                            model.lambda = queue_len as f64 * 0.1;
                            model.rho = model.lambda / model.mu;
                        }
                    }
                }

                let mut lb = load_balancer.write().await;
                for i in 0..worker_senders.len() {
                    if i < loads.len() {
                        lb.update_ema(i, loads[i]);
                    }
                }

                stats.insert("load_balancer_total_load".to_string(), (total_load * 100.0) as u64);
                stats.insert("load_balancer_total_throughput".to_string(), (total_throughput * 100.0) as u64);

                if !loads.is_empty() {
                    let avg_load = total_load / loads.len() as f64;
                    let imbalance = WorkStealingModel::compute_imbalance(&loads);

                    stats.insert("load_balancer_avg_load".to_string(), (avg_load * 100.0) as u64);
                    stats.insert("load_balancer_imbalance".to_string(), (imbalance * 1000.0) as u64);
                }
            }
        });
    }

    pub fn start_metrics_collector(&self) {
        let stats = self.stats.clone();
        let latency_histogram = self.latency_histogram.clone();
        let _worker_latency_history = self.worker_latency_history.clone();
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

    pub fn start_task_cleaner(&self) {
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

    pub async fn submit_task(&self, mut task: WorkStealingTask) -> Result<u64, BatchError> {
        // Проверка Circuit Breaker
        if !self.circuit_breaker.allow_request().await {
            self.circuit_breaker.record_failure().await;
            return Err(BatchError::ProcessingError("Circuit breaker is open".to_string()));
        }

        // Генерация ID
        let task_id = self.next_task_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        task.id = task_id;
        task.estimate_processing_time();

        if task.deadline.is_none() {
            task.deadline = Some(Instant::now() + Duration::from_secs(30));
        }

        // Проверка на перегрузку
        let total_backlog = self.worker_queues.iter()
            .map(|e| *e.value())
            .sum::<usize>() + self.injector_backlog.load(std::sync::atomic::Ordering::Relaxed);

        if total_backlog > self.backpressure_threshold * self.worker_senders.len() {
            self.circuit_breaker.record_failure().await;
            return Err(BatchError::Backpressure);
        }

        let lb = self.load_balancer.read().await;
        let (worker_idx, _) = lb.select_worker_power_of_two();
        drop(lb);

        // Попытка отправки в выбранный воркер
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
                // Fallback в injector
                match self.injector_sender.try_send(task) {
                    Ok(()) => {
                        self.injector_backlog.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                        self.stats.entry("tasks_submitted".to_string())
                            .and_modify(|e| *e += 1)
                            .or_insert(1);

                        self.stats.entry("injector_usage".to_string())
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

    pub fn get_result(&self, task_id: u64) -> Option<WorkStealingResult> {
        self.results.get(&task_id).map(|r| r.clone())
    }

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

        // Перцентили
        let mut latencies: Vec<u64> = self.latency_histogram.iter()
            .flat_map(|e| vec![*e.key(); *e.value() as usize])
            .collect();
        latencies.sort_unstable();

        let p95 = latencies.get(latencies.len() * 95 / 100).copied().unwrap_or(0) as f64;
        let p99 = latencies.get(latencies.len() * 99 / 100).copied().unwrap_or(0) as f64;

        // Метрики из других компонентов
        let current_batch_size = self.adaptive_batcher.get_batch_size().await;
        let circuit_state = self.circuit_breaker.get_state().await;
        let qos_quotas = self.qos_manager.get_quotas().await;
        let qos_utilization = self.qos_manager.get_utilization().await;

        // Здоровые воркеры
        let mut healthy_workers = 0;
        for i in 0..self.worker_senders.len() {
            let processed = stats.get(&format!("worker_{}_tasks", i)).copied().unwrap_or(0);
            if processed > 0 {
                healthy_workers += 1;
            }
        }

        // Дисбаланс
        let model = self.stealing_model.read().await;
        let imbalance = model.imbalance;
        let steal_probability = model.p_steal;
        drop(model);

        // Оптимальное количество воркеров
        let lambda_est = 100.0; // Будет заменяться реальным значением
        let optimal_workers = optimal_worker_count(lambda_est, 1000.0, 10.0);

        // Размеры очередей
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
            batch_metrics: None, // Заглушка, т.к. get_metrics отсутствует
            circuit_state,
            qos_quotas,
            qos_utilization,
            imbalance,
            queue_backlog,
            injector_backlog,
            steal_probability,
            optimal_workers,
            predicted_lambda: lambda_est,
            timestamp: Instant::now(),
        }
    }

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

    pub async fn shutdown(&self) {
        info!("🛑 Shutting down work-stealing dispatcher...");
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);

        self.worker_queues.clear();
        self.worker_loads.clear();
        self.worker_throughputs.clear();
        self.results.clear();
        self.stats.clear();
        self.latency_histogram.clear();
        self.worker_latency_history.clear();
        self.queue_models.clear();

        info!("✅ Work-stealing dispatcher stopped");
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
            worker_loads: self.worker_loads.clone(),
            worker_throughputs: self.worker_throughputs.clone(),
            injector_sender: self.injector_sender.clone(),
            injector_receiver: self.injector_receiver.clone(),
            injector_backlog: self.injector_backlog.clone(),
            results: self.results.clone(),
            stealing_model: self.stealing_model.clone(),
            load_balancer: self.load_balancer.clone(),
            queue_models: self.queue_models.clone(),
            stats: self.stats.clone(),
            latency_histogram: self.latency_histogram.clone(),
            worker_latency_history: self.worker_latency_history.clone(),
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
            check_rate: self.check_rate,
            cost_idle: self.cost_idle,
            cost_steal: self.cost_steal,
        }
    }
}