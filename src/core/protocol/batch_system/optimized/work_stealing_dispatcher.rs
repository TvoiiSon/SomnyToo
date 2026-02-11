use std::sync::Arc;
use std::time::{Instant, Duration};
use dashmap::DashMap;
use tracing::{info, debug, error};
use bytes::Bytes;
use flume::{Sender, Receiver, bounded};

use crate::core::protocol::batch_system::types::error::BatchError;
use crate::core::protocol::batch_system::types::priority::Priority;
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;

/// Задача для work-stealing диспетчера
#[derive(Debug, Clone)]
pub struct WorkStealingTask {
    pub id: u64,
    pub session_id: Vec<u8>,
    pub data: Bytes,
    pub source_addr: std::net::SocketAddr,
    pub priority: Priority,
    pub created_at: Instant,
    pub worker_id: Option<usize>,
}

/// Результат обработки
#[derive(Debug, Clone)]
pub struct WorkStealingResult {
    pub task_id: u64,
    pub session_id: Vec<u8>,
    pub result: Result<Vec<u8>, String>,
    pub processing_time: Duration,
    pub worker_id: usize,
    pub destination_addr: std::net::SocketAddr, // Добавлено
}

#[derive(Debug, Clone)]
pub struct WorkStealingDispatcherMetrics {
    pub worker_count: usize,
    pub total_tasks_submitted: u64,
    pub total_tasks_processed: u64,
    pub successful_decryptions: u64,
    pub failed_decryptions: u64,
    pub session_not_found: u64,
    pub work_steals: u64,
    pub timestamp: Instant,
}

/// Work-Stealing диспетчер с атомарными очередями
pub struct WorkStealingDispatcher {
    // Атомарные очереди для каждого worker'а
    worker_senders: Arc<Vec<Sender<WorkStealingTask>>>,
    worker_receivers: Arc<Vec<Receiver<WorkStealingTask>>>,

    // Канал для инжектора (для work stealing)
    injector_sender: Sender<WorkStealingTask>,
    injector_receiver: Receiver<WorkStealingTask>,

    // Concurrent хэш-таблица для результатов
    results: Arc<DashMap<u64, WorkStealingResult>>,

    // Статистика
    stats: Arc<DashMap<String, u64>>,

    // Управление
    is_running: Arc<std::sync::atomic::AtomicBool>,
    next_task_id: std::sync::atomic::AtomicU64,

    // ДОБАВИМ новые поля для обработки пакетов
    packet_processor: PhantomPacketProcessor,
    session_manager: Arc<PhantomSessionManager>,
}

impl WorkStealingDispatcher {
    pub fn new(
        num_workers: usize,
        queue_capacity: usize,
        session_manager: Arc<PhantomSessionManager>,
    ) -> Self {
        info!("🚀 Creating work-stealing dispatcher with {} workers and atomарными очередями", num_workers);

        let mut worker_senders = Vec::with_capacity(num_workers);
        let mut worker_receivers = Vec::with_capacity(num_workers);

        // Создаем атомарные каналы для каждого worker'а
        for _ in 0..num_workers {
            let (tx, rx) = bounded(queue_capacity);
            worker_senders.push(tx);
            worker_receivers.push(rx);
        }

        // Канал для инжектора (для work stealing)
        let (injector_sender, injector_receiver) = bounded(queue_capacity * 2);

        let dispatcher = Self {
            worker_senders: Arc::new(worker_senders),
            worker_receivers: Arc::new(worker_receivers),
            injector_sender,
            injector_receiver,
            results: Arc::new(DashMap::new()),
            stats: Arc::new(DashMap::new()),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            next_task_id: std::sync::atomic::AtomicU64::new(1),
            packet_processor: PhantomPacketProcessor::new(),
            session_manager,
        };

        // Запускаем worker'ов
        dispatcher.start_workers();

        dispatcher
    }

    /// Получение расширенных метрик (для совместимости с LoadAwareDispatcher)
    pub async fn get_advanced_metrics(&self) -> super::super::load_aware_dispatcher::AdvancedDispatcherMetrics {
        use super::super::circuit_breaker::CircuitState;

        // Получаем статистику
        let stats = self.get_stats();
        let total_tasks_processed: u64 = stats.get("total_tasks_processed").cloned().unwrap_or(0);
        let successful_decryptions: u64 = stats.get("successful_decryptions").cloned().unwrap_or(0);
        let _failed_decryptions: u64 = stats.get("failed_decryptions").cloned().unwrap_or(0);

        // Рассчитываем метрики
        let total_processed = total_tasks_processed;
        let _success_rate = if total_processed > 0 {
            successful_decryptions as f64 / total_processed as f64
        } else {
            0.0
        };

        // Собираем информацию о worker'ах
        let worker_count = self.worker_senders.len();
        let mut healthy_workers = 0;

        // Получаем статистику по каждому worker'у
        let mut worker_stats = Vec::new();
        for worker_id in 0..worker_count {
            let processed = stats.get(&format!("worker_{}_tasks", worker_id)).cloned().unwrap_or(0);
            worker_stats.push((worker_id, processed));
            if processed > 0 {
                healthy_workers += 1;
            }
        }

        // Рассчитываем imbalance (дисбаланс нагрузки)
        let imbalance = if !worker_stats.is_empty() {
            let total: u64 = worker_stats.iter().map(|(_, count)| count).sum();
            let avg = total as f64 / worker_count as f64;
            let variance: f64 = worker_stats.iter()
                .map(|(_, count)| (*count as f64 - avg).powi(2))
                .sum::<f64>() / worker_count as f64;
            variance.sqrt() / (avg + 1.0)
        } else {
            0.0
        };

        // Получаем среднее время обработки (упрощенно)
        let total_time = stats.get("processing_time_ms_total").cloned().unwrap_or(0);
        let avg_processing_time_ms = if total_processed > 0 {
            total_time as f64 / total_processed as f64
        } else {
            0.0
        };

        super::super::load_aware_dispatcher::AdvancedDispatcherMetrics {
            total_workers: worker_count,
            healthy_workers,
            total_queue: 0, // WorkStealingDispatcher не отслеживает очередь явно
            avg_processing_time_ms,
            circuit_breaker_state: CircuitState::Closed,
            qos_quotas: (0.0, 0.0, 0.0),
            qos_utilization: (0.0, 0.0, 0.0),
            current_batch_size: 0,
            batch_metrics: super::super::adaptive_batcher::BatchMetrics {
                total_batches: 0,
                total_items: 0,
                avg_batch_size: 0.0,
                avg_processing_time: Duration::from_secs(0),
                p95_processing_time: Duration::from_secs(0),
                p99_processing_time: Duration::from_secs(0),
                last_adaptation: Instant::now(),
                adaptation_count: 0,
            },
            imbalance,
        }
    }

    /// Получение расширенных метрик диспетчера
    pub async fn get_dispatcher_metrics(&self) -> WorkStealingDispatcherMetrics {
        let stats = self.get_stats();

        WorkStealingDispatcherMetrics {
            worker_count: self.worker_senders.len(),
            total_tasks_submitted: stats.get("tasks_submitted").cloned().unwrap_or(0),
            total_tasks_processed: stats.get("total_tasks_processed").cloned().unwrap_or(0),
            successful_decryptions: stats.get("successful_decryptions").cloned().unwrap_or(0),
            failed_decryptions: stats.get("failed_decryptions").cloned().unwrap_or(0),
            session_not_found: stats.get("session_not_found").cloned().unwrap_or(0),
            work_steals: stats.get("work_steals").cloned().unwrap_or(0),
            timestamp: Instant::now(),
        }
    }

    /// Запуск worker'ов
    fn start_workers(&self) {
        let num_workers = self.worker_senders.len();

        for worker_id in 0..num_workers {
            // Клонируем receiver для каждого worker'а
            let worker_receiver = self.worker_receivers[worker_id].clone();
            let injector_receiver = self.injector_receiver.clone();
            let results = self.results.clone();
            let stats = self.stats.clone();
            let is_running = self.is_running.clone();

            // Клонируем необходимые компоненты для дешифрования
            let packet_processor = self.packet_processor.clone();
            let session_manager = self.session_manager.clone();

            // Создаем отдельный task для каждого worker'а
            tokio::spawn(async move {
                Self::worker_loop(
                    worker_id,
                    worker_receiver,
                    injector_receiver,
                    results,
                    stats,
                    is_running,
                    packet_processor,
                    session_manager,
                ).await;
            });
        }

        info!("✅ Started {} work-stealing workers with atomарными очередями", num_workers);
    }

    async fn worker_loop(
        worker_id: usize,
        worker_receiver: Receiver<WorkStealingTask>,
        injector_receiver: Receiver<WorkStealingTask>,
        results: Arc<DashMap<u64, WorkStealingResult>>,
        stats: Arc<DashMap<String, u64>>,
        is_running: Arc<std::sync::atomic::AtomicBool>,
        packet_processor: PhantomPacketProcessor,
        session_manager: Arc<PhantomSessionManager>,
    ) {
        info!("👷 Work-stealing worker #{} started with atomарными очередями", worker_id);

        while is_running.load(std::sync::atomic::Ordering::Relaxed) {
            // Используем select! для работы с несколькими каналами
            tokio::select! {
                // 1. Пытаемся взять задачу из своей очереди
                Ok(task) = async { worker_receiver.recv_async().await } => {
                    Self::process_task_with_decryption(
                        worker_id,
                        task,
                        &results,
                        &stats,
                        &packet_processor,
                        &session_manager,
                    ).await;
                }

                // 2. Пытаемся взять задачу из инжектора (work stealing)
                Ok(task) = async { injector_receiver.recv_async().await } => {
                    *stats.entry("work_steals".to_string()).or_insert(0) += 1;
                    Self::process_task_with_decryption(
                        worker_id,
                        task,
                        &results,
                        &stats,
                        &packet_processor,
                        &session_manager,
                    ).await;
                }

                // 3. Проверяем необходимость остановки
                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    // Короткая пауза для снижения нагрузки на CPU
                    continue;
                }
            }
        }

        info!("👋 Work-stealing worker #{} stopped", worker_id);
    }

    /// Обработка задачи с дешифрованием
    async fn process_task_with_decryption(
        worker_id: usize,
        task: WorkStealingTask,
        results: &Arc<DashMap<u64, WorkStealingResult>>,
        stats: &Arc<DashMap<String, u64>>,
        packet_processor: &PhantomPacketProcessor,
        session_manager: &Arc<PhantomSessionManager>,
    ) {
        let start_time = Instant::now();

        // Получаем сессию асинхронно
        match session_manager.get_session(&task.session_id).await {
            Some(session) => {
                // Дешифруем данные через PhantomPacketProcessor
                match packet_processor.process_incoming_vec(&task.data, &session) {
                    Ok((packet_type, decrypted_data)) => {
                        // Формируем результат с пакетным типом и данными
                        let mut result_data = Vec::new();
                        result_data.push(packet_type);
                        result_data.extend_from_slice(&decrypted_data);

                        let result = WorkStealingResult {
                            task_id: task.id,
                            session_id: task.session_id,
                            result: Ok(result_data),
                            processing_time: start_time.elapsed(),
                            worker_id,
                            destination_addr: task.source_addr, // Добавлено
                        };

                        results.insert(task.id, result);

                        // Обновляем статистику
                        *stats.entry("total_tasks_processed".to_string()).or_insert(0) += 1;
                        *stats.entry(format!("worker_{}_tasks", worker_id)).or_insert(0) += 1;
                        *stats.entry("successful_decryptions".to_string()).or_insert(0) += 1;
                    }
                    Err(e) => {
                        error!("❌ Worker #{} decryption failed: {}", worker_id, e);

                        let result = WorkStealingResult {
                            task_id: task.id,
                            session_id: task.session_id,
                            result: Err(format!("Decryption failed: {}", e)),
                            processing_time: start_time.elapsed(),
                            worker_id,
                            destination_addr: task.source_addr, // Добавлено
                        };

                        results.insert(task.id, result);
                        *stats.entry("failed_decryptions".to_string()).or_insert(0) += 1;
                    }
                }
            }
            None => {
                let error = format!("Session not found: {}", hex::encode(&task.session_id));
                error!("❌ Worker #{}: {}", worker_id, error);

                let result = WorkStealingResult {
                    task_id: task.id,
                    session_id: task.session_id,
                    result: Err(error.clone()),
                    processing_time: start_time.elapsed(),
                    worker_id,
                    destination_addr: task.source_addr, // Добавлено
                };

                results.insert(task.id, result);
                *stats.entry("session_not_found".to_string()).or_insert(0) += 1;
            }
        }
    }

    /// Отправка задачи
    pub async fn submit_task(&self, mut task: WorkStealingTask) -> Result<u64, BatchError> {
        let task_id = self.next_task_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        task.id = task_id;

        // Выбор worker'а
        if let Some(target_worker_id) = task.worker_id {
            if target_worker_id < self.worker_senders.len() {
                // Пытаемся отправить в указанный worker
                match self.worker_senders[target_worker_id].send(task.clone()) {
                    Ok(_) => {
                        debug!("Task {} assigned to worker {}", task_id, target_worker_id);
                        *self.stats.entry("tasks_submitted".to_string()).or_insert(0) += 1;
                        return Ok(task_id);
                    }
                    Err(_) => {
                        // Очередь worker'а переполнена, отправляем в инжектор
                    }
                }
            }
        }

        // Round-robin распределение или отправка в инжектор при переполнении
        let worker_idx = task_id as usize % self.worker_senders.len();
        match self.worker_senders[worker_idx].try_send(task.clone()) {
            Ok(_) => {
                debug!("Task {} round-robin to worker {}", task_id, worker_idx);
            }
            Err(_) => {
                // Все worker'ы заняты, отправляем в инжектор
                match self.injector_sender.try_send(task) {
                    Ok(_) => debug!("Task {} sent to injector", task_id),
                    Err(_) => return Err(BatchError::Backpressure),
                }
            }
        }

        *self.stats.entry("tasks_submitted".to_string()).or_insert(0) += 1;
        Ok(task_id)
    }

    /// Получение результата
    pub fn get_result(&self, task_id: u64) -> Option<WorkStealingResult> {
        self.results.get(&task_id).map(|r| r.clone())
    }

    /// Получение статистики
    pub fn get_stats(&self) -> std::collections::HashMap<String, u64> {
        let mut stats_map = std::collections::HashMap::new();

        for entry in self.stats.iter() {
            stats_map.insert(entry.key().clone(), *entry.value());
        }

        stats_map
    }

    /// Остановка
    pub async fn shutdown(&self) {
        info!("🛑 Shutting down work-stealing dispatcher...");
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
        info!("✅ Work-stealing dispatcher stopped");
    }
}

impl Drop for WorkStealingDispatcher {
    fn drop(&mut self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}