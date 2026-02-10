use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::VecDeque;
use dashmap::DashMap;
use tracing::{info, debug, error};
use bytes::Bytes;
use tokio::sync::{mpsc, broadcast, Mutex};

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
}

/// Work-Stealing диспетчер
pub struct WorkStealingDispatcher {
    // Пары sender/receiver для каждого worker'а
    worker_senders: Arc<Vec<mpsc::Sender<WorkStealingTask>>>,
    worker_receivers: Arc<Vec<Mutex<mpsc::Receiver<WorkStealingTask>>>>,

    // Broadcast канал для инжектора
    injector_tx: broadcast::Sender<WorkStealingTask>,

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
        session_manager: Arc<PhantomSessionManager>, // ДОБАВИЛИ параметр
    ) -> Self {
        info!("🚀 Creating work-stealing dispatcher with {} workers and packet processing", num_workers);

        let mut worker_senders = Vec::with_capacity(num_workers);
        let mut worker_receivers = Vec::with_capacity(num_workers);

        // Создаем каналы для каждого worker'а
        for _ in 0..num_workers {
            let (tx, rx) = mpsc::channel(queue_capacity);
            worker_senders.push(tx);
            worker_receivers.push(Mutex::new(rx));
        }

        // Broadcast канал для инжектора
        let (injector_tx, _) = broadcast::channel(queue_capacity * 2);

        let dispatcher = Self {
            worker_senders: Arc::new(worker_senders),
            worker_receivers: Arc::new(worker_receivers),
            injector_tx,
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

    /// Получить расширенные метрики (для совместимости)
    pub async fn get_advanced_metrics(&self) -> super::super::load_aware_dispatcher::AdvancedDispatcherMetrics {
        use super::super::circuit_breaker::CircuitState;

        // Получаем базовые метрики
        let stats = self.get_stats();
        let _total_tasks: u64 = stats.values().sum();

        super::super::load_aware_dispatcher::AdvancedDispatcherMetrics {
            total_workers: self.worker_senders.len(),
            healthy_workers: self.worker_senders.len(), // Все worker'ы считаются здоровыми
            total_queue: 0, // WorkStealingDispatcher не отслеживает очередь
            avg_processing_time_ms: 0.0,
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
            imbalance: 0.0,
        }
    }

    fn process_task_with_decryption(
        worker_id: usize,
        task: WorkStealingTask,
        results: &Arc<DashMap<u64, WorkStealingResult>>,
        stats: &Arc<DashMap<String, u64>>,
        processed_count: &mut u64,
        packet_processor: &PhantomPacketProcessor,
        session_manager: &Arc<PhantomSessionManager>,
    ) {
        let start_time = Instant::now();

        // Пытаемся получить сессию - ВНИМАНИЕ: это блокирующий вызов
        // Нужно сделать его асинхронным или изменить архитектуру
        let session_result = {
            // Используем tokio::task::block_in_place для блокирующего вызова в async контексте
            tokio::task::block_in_place(|| {
                futures::executor::block_on(async {
                    session_manager.get_session(&task.session_id).await
                })
            })
        };

        match session_result {
            Some(session) => {
                // Дешифруем данные через PhantomPacketProcessor
                match packet_processor.process_incoming_vec(&task.data, &session) {
                    Ok((packet_type, decrypted_data)) => {
                        info!("✅ Worker #{} decrypted packet type: 0x{:02x}, data: {} bytes",
                          worker_id, packet_type, decrypted_data.len());

                        // Формируем результат с пакетным типом и данными
                        let mut result_data = Vec::new();
                        result_data.push(packet_type); // Тип пакета как первый байт
                        result_data.extend_from_slice(&decrypted_data);

                        let result = WorkStealingResult {
                            task_id: task.id,
                            session_id: task.session_id,
                            result: Ok(result_data), // Возвращаем данные для дальнейшей обработки
                            processing_time: start_time.elapsed(),
                            worker_id,
                        };

                        results.insert(task.id, result);
                        *processed_count += 1;

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
                        };

                        results.insert(task.id, result);
                        *processed_count += 1;

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
                };

                results.insert(task.id, result);
                *processed_count += 1;

                *stats.entry("session_not_found".to_string()).or_insert(0) += 1;
            }
        }
    }

    fn start_workers(&self) {
        let num_workers = self.worker_senders.len();

        for worker_id in 0..num_workers {
            let worker_receivers = self.worker_receivers.clone();
            let injector_rx = self.injector_tx.subscribe();
            let results = self.results.clone();
            let stats = self.stats.clone();
            let is_running = self.is_running.clone();

            // Клонируем необходимые компоненты для дешифрования
            let packet_processor = self.packet_processor.clone();
            let session_manager = self.session_manager.clone();

            // Создаем отдельный task для каждого worker'а
            let worker_task = async move {
                Self::worker_loop(
                    worker_id,
                    worker_receivers,
                    injector_rx,
                    results,
                    stats,
                    is_running,
                    packet_processor,
                    session_manager,
                ).await;
            };

            tokio::spawn(worker_task);
        }

        info!("✅ Started {} work-stealing workers with packet processing", num_workers);
    }

    async fn worker_loop(
        worker_id: usize,
        worker_receivers: Arc<Vec<Mutex<mpsc::Receiver<WorkStealingTask>>>>,
        mut injector_rx: broadcast::Receiver<WorkStealingTask>,
        results: Arc<DashMap<u64, WorkStealingResult>>,
        stats: Arc<DashMap<String, u64>>,
        is_running: Arc<std::sync::atomic::AtomicBool>,

        // ДОБАВИМ новые параметры для дешифрования
        packet_processor: PhantomPacketProcessor,
        session_manager: Arc<PhantomSessionManager>,
    ) {
        info!("👷 Work-stealing worker #{} started with packet processing", worker_id);

        let mut processed_count = 0;
        let mut steal_count = 0;
        let mut local_queue = VecDeque::new();

        while is_running.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::select! {
            // 1. Пытаемся взять задачу из своей очереди
            task = async {
                if let Some(receiver_mutex) = worker_receivers.get(worker_id) {
                    let mut receiver = receiver_mutex.lock().await;
                    receiver.recv().await
                } else {
                    None
                }
            } => {
                if let Some(task) = task {
                    local_queue.push_back(task);
                }
            }

            // 2. Пытаемся взять задачу из инжектора
            Ok(task) = injector_rx.recv() => {
                local_queue.push_back(task);
                steal_count += 1;
            }

            // 3. Work-stealing
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                let num_workers = worker_receivers.len();
                for offset in 1..num_workers {
                    let victim_id = (worker_id + offset) % num_workers;

                    if let Some(victim_mutex) = worker_receivers.get(victim_id) {
                        let mut victim_receiver = victim_mutex.lock().await;
                        if let Ok(task) = victim_receiver.try_recv() {
                            local_queue.push_back(task);
                            *stats.entry("steals".to_string()).or_insert(0) += 1;
                            break;
                        }
                    }
                }
            }
        }

            // Обрабатываем задачи с ДЕШИФРОВАНИЕМ
            while let Some(task) = local_queue.pop_front() {
                // ИСПОЛЬЗУЕМ НОВЫЙ МЕТОД С ДЕШИФРОВАНИЕМ
                Self::process_task_with_decryption(
                    worker_id,
                    task,
                    &results,
                    &stats,
                    &mut processed_count,
                    &packet_processor,
                    &session_manager,
                );
            }

            // Обновляем статистику
            if processed_count >= 100 {
                stats.insert(format!("worker_{}_processed", worker_id), processed_count as u64);
                stats.insert(format!("worker_{}_steals", worker_id), steal_count as u64);
                processed_count = 0;
                steal_count = 0;
            }

            // Пауза
            tokio::time::sleep(Duration::from_micros(10)).await;
        }

        info!("👋 Work-stealing worker #{} stopped", worker_id);
    }

    // fn process_task(
    //     worker_id: usize,
    //     task: WorkStealingTask,
    //     results: &Arc<DashMap<u64, WorkStealingResult>>,
    //     stats: &Arc<DashMap<String, u64>>,
    //     processed_count: &mut u64,
    // ) {
    //     let start_time = Instant::now();
    //
    //     // Симуляция обработки
    //     let result = if !task.data.is_empty() {
    //         Ok(format!("Processed by worker {}: {} bytes", worker_id, task.data.len()).into_bytes())
    //     } else {
    //         Err("Empty data".to_string())
    //     };
    //
    //     let processing_time = start_time.elapsed();
    //
    //     let task_result = WorkStealingResult {
    //         task_id: task.id,
    //         session_id: task.session_id,
    //         result,
    //         processing_time,
    //         worker_id,
    //     };
    //
    //     results.insert(task.id, task_result);
    //     *processed_count += 1;
    //
    //     // Обновляем статистику
    //     *stats.entry("total_tasks_processed".to_string()).or_insert(0) += 1;
    //     *stats.entry(format!("worker_{}_tasks", worker_id)).or_insert(0) += 1;
    //
    //     trace!("Worker #{} processed task {} in {:?}", worker_id, task.id, processing_time);
    // }

    /// Отправка задачи
    pub async fn submit_task(&self, task: WorkStealingTask) -> Result<u64, BatchError> {
        let task_id = self.next_task_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let task = WorkStealingTask {
            id: task_id,
            session_id: task.session_id,
            data: task.data,
            source_addr: task.source_addr,
            priority: task.priority,
            created_at: Instant::now(),
            worker_id: task.worker_id,
        };

        // Выбор worker'а
        if let Some(target_worker_id) = task.worker_id {
            if target_worker_id < self.worker_senders.len() {
                let sender = &self.worker_senders[target_worker_id];
                match sender.send(task.clone()).await {
                    Ok(_) => debug!("Task {} assigned to worker {}", task_id, target_worker_id),
                    Err(_) => {
                        // Отправляем в broadcast
                        if let Err(e) = self.injector_tx.send(task) {
                            return Err(BatchError::ProcessingError(format!("Failed to send task: {}", e)));
                        }
                        debug!("Task {} sent to broadcast", task_id);
                    }
                }
            } else {
                // Отправляем в broadcast
                if let Err(e) = self.injector_tx.send(task) {
                    return Err(BatchError::ProcessingError(format!("Failed to send task: {}", e)));
                }
            }
        } else {
            // Round-robin
            let worker_idx = task_id as usize % self.worker_senders.len();
            let sender = &self.worker_senders[worker_idx];
            match sender.send(task.clone()).await {
                Ok(_) => debug!("Task {} round-robin to worker {}", task_id, worker_idx),
                Err(_) => {
                    // Отправляем в broadcast
                    if let Err(e) = self.injector_tx.send(task) {
                        return Err(BatchError::ProcessingError(format!("Failed to send task: {}", e)));
                    }
                    debug!("Task {} sent to broadcast", task_id);
                }
            }
        }

        // Статистика
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