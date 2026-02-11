use std::sync::Arc;
use std::time::{Instant, Duration};
use dashmap::DashMap;
use tracing::{info, debug, error, warn};
use bytes::Bytes;
use flume::{Sender, Receiver, bounded};

use crate::core::protocol::batch_system::types::error::BatchError;
use crate::core::protocol::batch_system::types::priority::Priority;
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;

use super::circuit_breaker::{CircuitBreaker, CircuitState};
use super::qos_manager::QosManager;
use super::adaptive_batcher::AdaptiveBatcher;

use crate::core::protocol::batch_system::optimized::work_stealing_dispatcher::WorkStealingResult;

/// Задача с учетом нагрузки
#[derive(Debug, Clone)]
pub struct LoadAwareTask {
    pub id: u64,
    pub session_id: Vec<u8>,
    pub data: Bytes,
    pub source_addr: std::net::SocketAddr,
    pub priority: Priority,
    pub created_at: Instant,
    pub estimated_complexity: u32,
    pub deadline: Option<Instant>,
}

/// Информация о загрузке worker'а
#[derive(Debug, Clone)]
pub struct WorkerLoadInfo {
    pub worker_id: usize,
    pub queue_size: usize,
    pub processing_rate: f64,
    pub avg_processing_time: Duration,
    pub last_update: Instant,
    pub is_healthy: bool,
    pub current_complexity: u32,
}

/// Диспетчер с учетом нагрузки с атомарными очередями
pub struct LoadAwareDispatcher {
    // Атомарные каналы для worker'ов
    worker_senders: Arc<Vec<Sender<LoadAwareTask>>>,
    worker_receivers: Arc<Vec<Receiver<LoadAwareTask>>>,

    // Канал для инжектора
    injector_sender: Sender<LoadAwareTask>,
    injector_receiver: Receiver<LoadAwareTask>,

    results: Arc<DashMap<u64, WorkStealingResult>>,
    stats: Arc<DashMap<String, u64>>,

    // Новые компоненты
    worker_loads: Arc<parking_lot::RwLock<Vec<WorkerLoadInfo>>>,
    circuit_breaker: Arc<CircuitBreaker>,
    qos_manager: Arc<QosManager>,
    adaptive_batcher: Arc<AdaptiveBatcher>,

    // Управление
    is_running: Arc<std::sync::atomic::AtomicBool>,
    next_task_id: std::sync::atomic::AtomicU64,

    // Метрики
    metrics: Arc<DashMap<String, f64>>,
}

impl LoadAwareDispatcher {
    /// Запуск worker'ов с атомарными очередями
    fn start_workers(&self, session_manager: Arc<PhantomSessionManager>) {
        let packet_processor = PhantomPacketProcessor::new();
        let num_workers = self.worker_senders.len();

        for worker_id in 0..num_workers {
            let worker_receiver = self.worker_receivers[worker_id].clone();
            let injector_receiver = self.injector_receiver.clone();
            let results = self.results.clone();
            let stats = self.stats.clone();
            let is_running = self.is_running.clone();
            let worker_loads = self.worker_loads.clone();

            let packet_processor_clone = packet_processor.clone();
            let session_manager_clone = session_manager.clone();

            tokio::spawn(async move {
                Self::worker_loop(
                    worker_id,
                    worker_receiver,
                    injector_receiver,
                    results,
                    stats,
                    is_running,
                    worker_loads,
                    packet_processor_clone,
                    session_manager_clone,
                ).await;
            });
        }

        info!("👷 Запущено {} worker'ов с атомарными очередями", num_workers);
    }

    async fn worker_loop(
        worker_id: usize,
        worker_receiver: Receiver<LoadAwareTask>,
        injector_receiver: Receiver<LoadAwareTask>,
        results: Arc<DashMap<u64, WorkStealingResult>>,
        stats: Arc<DashMap<String, u64>>,
        is_running: Arc<std::sync::atomic::AtomicBool>,
        worker_loads: Arc<parking_lot::RwLock<Vec<WorkerLoadInfo>>>,
        _packet_processor: PhantomPacketProcessor,
        _session_manager: Arc<PhantomSessionManager>,
    ) {
        info!("👷 Worker #{} запущен с атомарными очередями", worker_id);

        while is_running.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::select! {
                // Берем из своей очереди
                Ok(task) = worker_receiver.recv_async() => {
                    Self::process_task(
                        worker_id,
                        task,
                        &results,
                        &stats,
                        &worker_loads,
                    ).await;
                }

                // Work-stealing из инжектора
                Ok(task) = injector_receiver.recv_async() => {
                    *stats.entry("work_steals".to_string()).or_insert(0) += 1;
                    Self::process_task(
                        worker_id,
                        task,
                        &results,
                        &stats,
                        &worker_loads,
                    ).await;
                }

                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    // Короткая пауза
                }
            }
        }

        info!("👋 Worker #{} остановлен", worker_id);
    }

    async fn process_task(
        worker_id: usize,
        task: LoadAwareTask,
        results: &Arc<DashMap<u64, WorkStealingResult>>,
        stats: &Arc<DashMap<String, u64>>,
        worker_loads: &Arc<parking_lot::RwLock<Vec<WorkerLoadInfo>>>,
    ) {
        let start_time = Instant::now();

        // Имитация обработки задачи
        tokio::time::sleep(Duration::from_millis(1)).await;

        let processing_time = start_time.elapsed();

        // Обновляем метрики загрузки
        {
            let mut loads = worker_loads.write();
            if let Some(worker) = loads.get_mut(worker_id) {
                worker.queue_size = worker.queue_size.saturating_sub(1);
                worker.current_complexity = worker.current_complexity.saturating_sub(task.estimated_complexity);
                worker.avg_processing_time = Duration::from_nanos(
                    (worker.avg_processing_time.as_nanos() as f64 * 0.9 +
                        processing_time.as_nanos() as f64 * 0.1) as u64
                );
                worker.last_update = Instant::now();
            }
        }

        // Сохраняем результат
        let result = WorkStealingResult {
            task_id: task.id,
            session_id: task.session_id.clone(),
            result: Ok(vec![]), // Упрощенный результат
            processing_time,
            worker_id,
            destination_addr: task.source_addr, // Добавлено
        };

        results.insert(task.id, result);

        // Обновляем статистику
        *stats.entry(format!("worker_{}_processed", worker_id)).or_insert(0) += 1;
        *stats.entry("total_tasks_processed".to_string()).or_insert(0) += 1;
    }

    /// Мониторинг загрузки worker'ов
    fn start_load_monitor(&self) {
        let worker_loads = self.worker_loads.clone();
        let is_running = self.is_running.clone();
        let metrics = self.metrics.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let loads = worker_loads.read();

                let total_queue: usize = loads.iter().map(|w| w.queue_size).sum();
                let avg_processing_time: Duration = loads.iter()
                    .map(|w| w.avg_processing_time)
                    .sum::<Duration>() / loads.len().max(1) as u32;

                let healthy_workers = loads.iter().filter(|w| w.is_healthy).count();
                let unhealthy_workers = loads.len() - healthy_workers;

                metrics.insert("dispatcher.total_queue".to_string(), total_queue as f64);
                metrics.insert("dispatcher.avg_processing_time_ms".to_string(),
                               avg_processing_time.as_millis() as f64);
                metrics.insert("dispatcher.healthy_workers".to_string(), healthy_workers as f64);
                metrics.insert("dispatcher.unhealthy_workers".to_string(), unhealthy_workers as f64);

                if !loads.is_empty() {
                    let avg_load = total_queue as f64 / loads.len() as f64;
                    let variance: f64 = loads.iter()
                        .map(|w| (w.queue_size as f64 - avg_load).powi(2))
                        .sum::<f64>() / loads.len() as f64;
                    let imbalance = variance.sqrt();

                    metrics.insert("dispatcher.load_imbalance".to_string(), imbalance);
                }
            }
        });
    }

    /// Сборщик метрик
    fn start_metrics_collector(&self) {
        let is_running = self.is_running.clone();
        let metrics = self.metrics.clone();
        let stats = self.stats.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let total_tasks: u64 = stats.iter()
                    .filter(|e| e.key().starts_with("worker_") && e.key().ends_with("_processed"))
                    .map(|e| *e.value())
                    .sum();

                metrics.insert("dispatcher.total_tasks".to_string(), total_tasks as f64);
            }
        });
    }

    /// Выбор оптимального worker'а
    async fn select_optimal_worker(&self, task: &LoadAwareTask) -> Option<usize> {
        let loads = self.worker_loads.read();

        if loads.is_empty() {
            return None;
        }

        match task.priority {
            Priority::Critical => {
                loads.iter()
                    .filter(|w| w.is_healthy)
                    .min_by_key(|w| w.queue_size)
                    .map(|w| w.worker_id)
            }
            Priority::High => {
                loads.iter()
                    .filter(|w| w.is_healthy)
                    .min_by(|a, b| {
                        let score_a = a.queue_size as f64 + a.current_complexity as f64 * 0.1;
                        let score_b = b.queue_size as f64 + b.current_complexity as f64 * 0.1;
                        score_a.partial_cmp(&score_b).unwrap()
                    })
                    .map(|w| w.worker_id)
            }
            _ => {
                let current_id = task.id as usize % loads.len();
                for offset in 0..loads.len() {
                    let idx = (current_id + offset) % loads.len();
                    if loads[idx].is_healthy {
                        return Some(idx);
                    }
                }
                None
            }
        }
    }

    fn estimate_task_complexity(&self, task: &LoadAwareTask) -> u32 {
        let base_complexity = match task.priority {
            Priority::Critical => 10,
            Priority::High => 8,
            Priority::Normal => 5,
            Priority::Low => 2,
            Priority::Background => 1,
        };

        let size_factor = (task.data.len() / 1024).min(10) as u32;
        let crypto_factor = 3;

        base_complexity + size_factor + crypto_factor
    }

    fn calculate_imbalance(&self, loads: &[WorkerLoadInfo]) -> f64 {
        if loads.is_empty() {
            return 0.0;
        }

        let total_load: usize = loads.iter().map(|w| w.queue_size).sum();
        let avg_load = total_load as f64 / loads.len() as f64;

        let variance: f64 = loads.iter()
            .map(|w| (w.queue_size as f64 - avg_load).powi(2))
            .sum::<f64>() / loads.len() as f64;

        variance.sqrt() / (avg_load + 1.0)
    }
}

#[derive(Debug, Clone)]
pub struct AdvancedDispatcherMetrics {
    pub total_workers: usize,
    pub healthy_workers: usize,
    pub total_queue: usize,
    pub avg_processing_time_ms: f64,
    pub circuit_breaker_state: CircuitState,
    pub qos_quotas: (f64, f64, f64),
    pub qos_utilization: (f64, f64, f64),
    pub current_batch_size: usize,
    pub batch_metrics: super::adaptive_batcher::BatchMetrics,
    pub imbalance: f64,
}