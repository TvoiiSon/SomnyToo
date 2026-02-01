use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::{HashMap, VecDeque};
use tokio::sync::{mpsc, RwLock, Mutex, Semaphore, Notify};
use tracing::{info, debug, warn, error, trace};

pub(crate) use super::config::PacketBatchDispatcherConfig;
use super::task::{DispatchTask, TaskType};
use super::priority::DispatchPriority;
use super::worker::{WorkerState, WorkerHandle};
use super::stats::DispatcherStats;
use crate::core::protocol::phantom_crypto::batch::processor::crypto_batch_processor::CryptoBatchProcessor;
use crate::core::protocol::phantom_crypto::batch::io::writer::batch_writer::BatchWriter;
use crate::core::protocol::phantom_crypto::batch::io::reader::batch_reader::{BatchReaderEvent, BatchFrame};
use crate::core::monitoring::unified_monitor::{UnifiedMonitor};
use crate::core::protocol::phantom_crypto::batch::types::error::BatchError;

/// Пакетный диспетчер пакетов
pub struct PacketBatchDispatcher {
    config: PacketBatchDispatcherConfig,
    crypto_processor: Arc<CryptoBatchProcessor>,
    batch_writer: Arc<BatchWriter>,
    monitor: Arc<UnifiedMonitor>,

    // Очереди задач
    priority_queues: Arc<RwLock<Vec<VecDeque<DispatchTask>>>>,
    task_registry: Arc<RwLock<HashMap<u64, DispatchTask>>>,

    // Worker-ы
    workers: Arc<RwLock<HashMap<usize, WorkerHandle>>>,
    worker_states: Arc<RwLock<HashMap<usize, WorkerState>>>,

    // Каналы коммуникации
    task_tx: mpsc::Sender<DispatchTask>,
    task_rx: Mutex<mpsc::Receiver<DispatchTask>>,
    result_tx: mpsc::Sender<crate::core::protocol::phantom_crypto::batch::types::result::DispatchResult>,
    result_rx: Mutex<mpsc::Receiver<crate::core::protocol::phantom_crypto::batch::types::result::DispatchResult>>,

    // Управление
    shutdown_notify: Arc<Notify>,
    backpressure_semaphore: Arc<Semaphore>,

    // Статистика
    stats: Mutex<DispatcherStats>,
    task_counter: std::sync::atomic::AtomicU64,
    batch_counter: std::sync::atomic::AtomicU64,

    // Work stealing
    work_stealing_enabled: bool,
    steal_attempts: std::sync::atomic::AtomicUsize,
}

impl PacketBatchDispatcher {
    /// Создание нового диспетчера
    pub async fn new(
        config: PacketBatchDispatcherConfig,
        crypto_processor: Arc<CryptoBatchProcessor>,
        batch_writer: Arc<BatchWriter>,
        monitor: Arc<UnifiedMonitor>,
    ) -> Self {
        // Создаем приоритетные очереди
        let mut priority_queues = Vec::with_capacity(config.priority_queues);
        for _ in 0..config.priority_queues {
            priority_queues.push(VecDeque::new());
        }

        // Каналы для задач и результатов
        let (task_tx, task_rx) = mpsc::channel(config.max_queue_size);
        let (result_tx, result_rx) = mpsc::channel(1000);

        let dispatcher = Self {
            config: config.clone(),
            crypto_processor: crypto_processor.clone(),
            batch_writer: batch_writer.clone(),
            monitor: monitor.clone(),
            priority_queues: Arc::new(RwLock::new(priority_queues)),
            task_registry: Arc::new(RwLock::new(HashMap::new())),
            workers: Arc::new(RwLock::new(HashMap::new())),
            worker_states: Arc::new(RwLock::new(HashMap::new())),
            task_tx: task_tx.clone(), // Сохраняем
            task_rx: Mutex::new(task_rx), // Сохраняем
            result_tx: result_tx.clone(),
            result_rx: Mutex::new(result_rx),
            shutdown_notify: Arc::new(Notify::new()),
            backpressure_semaphore: Arc::new(Semaphore::new(config.max_queue_size)),
            stats: Mutex::new(DispatcherStats::new()),
            task_counter: std::sync::atomic::AtomicU64::new(0),
            batch_counter: std::sync::atomic::AtomicU64::new(0), // Сохраняем
            work_stealing_enabled: config.enable_work_stealing,
            steal_attempts: std::sync::atomic::AtomicUsize::new(0),
        };

        // Запускаем worker-ов
        for worker_id in 0..config.worker_count {
            dispatcher.spawn_worker(worker_id).await;
        }

        // Запускаем обработчик результатов
        dispatcher.start_result_handler().await;

        // Запускаем балансировщик нагрузки
        dispatcher.start_load_balancer().await;

        info!("🚀 PacketBatchDispatcher initialized with {} workers", config.worker_count);

        dispatcher
    }

    /// Обработка входящего батча от BatchReader
    pub async fn process_batch_from_reader(&self, batch_event: BatchReaderEvent) {
        match batch_event {
            BatchReaderEvent::BatchReady { batch_id, frames, source_addr, received_at } => {
                debug!("📦 Processing batch #{} from reader: {} frames", batch_id, frames.len());

                // Распределяем фреймы по задачам
                for frame in &frames {
                    self.create_dispatch_task(frame.clone(), source_addr, received_at).await;
                }

                // Обновляем статистику
                self.update_batch_stats(frames.len()).await;
            }
            BatchReaderEvent::ConnectionClosed { source_addr, reason } => {
                warn!("Connection closed: {} - {}", source_addr, reason);
                self.handle_connection_closed(source_addr).await;
            }
            BatchReaderEvent::ReadError { source_addr, error } => {
                error!("Read error from {}: {}", source_addr, error);
                self.handle_read_error(source_addr, error).await;
            }
            BatchReaderEvent::StatisticsUpdate { stats } => {
                trace!("Reader stats update: {} fps", stats.frames_per_second);
            }
        }
    }

    /// Создание задачи диспетчеризации из фрейма
    async fn create_dispatch_task(&self, frame: BatchFrame, source_addr: std::net::SocketAddr, received_at: Instant) {
        let task_id = self.task_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Определяем тип задачи на основе данных фрейма
        let task_type = self.determine_task_type(&frame.data);

        // Определяем приоритет
        let priority = DispatchPriority::from(frame.priority);

        let task = DispatchTask {
            task_id,
            session_id: frame.session_id,
            data: frame.data.to_vec(),
            source_addr,
            received_at,
            priority,
            task_type,
        };

        // Регистрируем задачу
        {
            let mut registry = self.task_registry.write().await;
            registry.insert(task_id, task.clone());
        }

        // Ставим задачу в очередь
        self.enqueue_task(task).await;
    }

    pub fn get_batch_count(&self) -> u64 {
        self.batch_counter.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn increment_batch_counter(&self) -> u64 {
        self.batch_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Определение типа задачи на основе данных
    fn determine_task_type(&self, data: &[u8]) -> TaskType {
        if data.is_empty() {
            return TaskType::Processing;
        }

        // Heartbeat пакеты
        if data[0] == 0x10 {
            return TaskType::Heartbeat;
        }

        // Зашифрованные пакеты требуют дешифрования
        if data.len() > 2 && data[0] == 0xAB && data[1] == 0xCE {
            return TaskType::Decryption;
        }

        // Остальное - обработка
        TaskType::Processing
    }

    /// Постановка задачи в очередь
    async fn enqueue_task(&self, task: DispatchTask) {
        let priority_index = task.priority as usize % self.config.priority_queues;

        // Проверяем backpressure
        if self.backpressure_semaphore.available_permits() == 0 {
            let mut stats = self.stats.lock().await;
            stats.backpressure_events += 1;

            // Аварийный flush если очередь переполнена
            if self.should_emergency_flush().await {
                self.emergency_flush().await;
            }

            warn!("Backpressure in dispatcher, dropping task {}", task.task_id);
            return;
        }

        // Забираем permit
        let _permit = self.backpressure_semaphore.clone().try_acquire_owned().ok();

        // Добавляем задачу в приоритетную очередь
        {
            let mut queues = self.priority_queues.write().await;
            if priority_index < queues.len() {
                // Вставляем в соответствии с приоритетом
                let queue = &mut queues[priority_index];

                // Находим позицию для вставки (сортировка по возрастанию приоритета)
                let mut insert_pos = 0;
                for (i, existing_task) in queue.iter().enumerate() {
                    if task.priority < existing_task.priority {
                        insert_pos = i;
                        break;
                    } else {
                        insert_pos = i + 1;
                    }
                }

                if insert_pos >= queue.len() {
                    queue.push_back(task.clone());
                } else {
                    // Создаем новую задачу для вставки
                    let task_clone = task.clone();
                    let tasks_after: Vec<_> = queue.drain(insert_pos..).collect();
                    queue.push_back(task_clone);
                    for t in tasks_after {
                        queue.push_back(t);
                    }
                }
            }
        }

        // Обновляем статистику
        {
            let mut stats = self.stats.lock().await;
            stats.task_received(task.priority);
        }

        // Уведомляем worker-ов о новой задаче
        self.shutdown_notify.notify_one();
    }

    /// Метод для внешней отправки задач в диспетчер
    pub async fn submit_task(&self, task: DispatchTask) -> Result<(), BatchError> {
        // Используем task_tx для отправки задачи
        match self.task_tx.send(task).await {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("Failed to submit task to dispatcher: {}", e);
                Err(BatchError::ChannelError(e.to_string()))
            }
        }
    }

    /// Метод для получения задач напрямую (для тестирования)
    pub async fn receive_task(&self) -> Option<DispatchTask> {
        let mut task_rx = self.task_rx.lock().await;
        task_rx.recv().await
    }

    /// Метод для проверки размера очереди задач
    pub async fn task_queue_size(&self) -> usize {
        // Можно добавить дополнительную логику для мониторинга
        let stats = self.stats.lock().await;
        stats.total_tasks_received as usize - stats.total_tasks_processed as usize
    }

    /// Запуск worker-а
    async fn spawn_worker(&self, worker_id: usize) {
        let dispatcher = self.clone();
        let (worker_task_tx, mut worker_task_rx) = mpsc::channel::<DispatchTask>(100);

        let join_handle = tokio::spawn(async move {
            info!("👷 Dispatcher worker #{} started", worker_id);

            let mut processed_count = 0;
            let mut current_batch: HashMap<DispatchPriority, Vec<DispatchTask>> = HashMap::new();

            while let Some(task) = worker_task_rx.recv().await {
                // Добавляем задачу в текущий батч
                let priority = task.priority;
                current_batch.entry(priority)
                    .or_insert_with(Vec::new)
                    .push(task);

                // Проверяем, готов ли батч к обработке
                let batch_size_limit = dispatcher.config.batch_size_per_priority
                    .get(&crate::core::protocol::phantom_crypto::batch::types::priority::BatchPriority::from(priority))
                    .copied()
                    .unwrap_or(64);

                if current_batch.get(&priority).map(|v| v.len()).unwrap_or(0) >= batch_size_limit {
                    // Обрабатываем батч
                    dispatcher.process_batch(worker_id, priority, &mut current_batch).await;
                }

                processed_count += 1;

                // Периодически логируем статистику
                if processed_count % 100 == 0 {
                    debug!("Worker #{} processed {} tasks", worker_id, processed_count);
                }
            }

            // Обрабатываем оставшиеся задачи при shutdown
            for (priority, tasks) in current_batch.drain() {
                if !tasks.is_empty() {
                    let mut remaining_batch = HashMap::new();
                    remaining_batch.insert(priority, tasks);
                    dispatcher.process_batch(worker_id, priority, &mut remaining_batch).await;
                }
            }

            info!("👷 Dispatcher worker #{} stopped", worker_id);
        });

        // Сохраняем handle worker-а
        let worker_handle = WorkerHandle::new(
            worker_id,
            join_handle,
            worker_task_tx,
            self.shutdown_notify.clone(),
        );

        {
            let mut workers = self.workers.write().await;
            workers.insert(worker_id, worker_handle);
        }

        // Инициализируем состояние worker-а
        let worker_state = WorkerState::new(worker_id);

        {
            let mut worker_states = self.worker_states.write().await;
            worker_states.insert(worker_id, worker_state);
        }
    }

    /// Обработка батча задач
    async fn process_batch(
        &self,
        worker_id: usize,
        priority: DispatchPriority,
        current_batch: &mut HashMap<DispatchPriority, Vec<DispatchTask>>,
    ) {
        let batch_start = Instant::now();

        if let Some(tasks) = current_batch.remove(&priority) {
            if tasks.is_empty() {
                return;
            }

            let batch_size = tasks.len();

            self.increment_batch_counter();

            debug!("Worker #{} processing {:?} batch of {} tasks",
                   worker_id, priority, batch_size);

            // Группируем задачи по типам
            let mut decryption_tasks = Vec::new();
            let mut encryption_tasks = Vec::new();
            let mut processing_tasks = Vec::new();
            let mut heartbeat_tasks = Vec::new();

            for task in tasks {
                match task.task_type {
                    TaskType::Decryption => decryption_tasks.push(task),
                    TaskType::Encryption => encryption_tasks.push(task),
                    TaskType::Processing => processing_tasks.push(task),
                    TaskType::Heartbeat => heartbeat_tasks.push(task),
                }
            }

            // Обрабатываем каждый тип задач
            let mut all_results = Vec::new();

            if !decryption_tasks.is_empty() {
                let decryption_results = self.process_decryption_batch(
                    worker_id, &decryption_tasks
                ).await;
                all_results.extend(decryption_results);
            }

            if !encryption_tasks.is_empty() {
                let encryption_results = self.process_encryption_batch(
                    worker_id, &encryption_tasks
                ).await;
                all_results.extend(encryption_results);
            }

            if !processing_tasks.is_empty() {
                let processing_results = self.process_processing_batch(
                    worker_id, &processing_tasks
                ).await;
                all_results.extend(processing_results);
            }

            if !heartbeat_tasks.is_empty() {
                let heartbeat_results = self.process_heartbeat_batch(
                    worker_id, &heartbeat_tasks
                ).await;
                all_results.extend(heartbeat_results);
            }

            // Отправляем результаты
            for result in all_results {
                self.send_result(result).await;
            }

            // Обновляем статистику worker-а
            self.update_worker_state(worker_id, batch_size, batch_start.elapsed()).await;

            // Обновляем общую статистику
            self.update_dispatcher_stats(batch_size, batch_start.elapsed()).await;

            debug!("Worker #{} completed {:?} batch in {:?}",
                   worker_id, priority, batch_start.elapsed());
        }
    }

    /// Обработка батча дешифрования
    async fn process_decryption_batch(
        &self,
        worker_id: usize,
        tasks: &[DispatchTask],
    ) -> Vec<crate::core::protocol::phantom_crypto::batch::types::result::DispatchResult> {
        let mut results = Vec::with_capacity(tasks.len());

        // Получаем буфер для обработки
        let buffer_index = 0; // Временная заглушка - для реальной реализации нужно управление буферами

        // Обработка дешифрования
        for task in tasks {
            let result = self.crypto_processor.process_single_decryption(
                None,
                &task.data,
                0, // expected_sequence
                buffer_index,
            );

            let dispatch_result = crate::core::protocol::phantom_crypto::batch::types::result::DispatchResult {
                task_id: task.task_id,
                session_id: task.session_id.clone(),
                result: match result {
                    Ok((_packet_type, decrypted_data)) => Ok(decrypted_data),
                    Err(e) => Err(format!("Decryption error: {}", e)),
                },
                processing_time: task.received_at.elapsed(),
                worker_id,
                priority: task.priority,
            };

            results.push(dispatch_result);
        }

        results
    }

    /// Обработка батча шифрования
    async fn process_encryption_batch(
        &self,
        worker_id: usize,
        tasks: &[DispatchTask],
    ) -> Vec<crate::core::protocol::phantom_crypto::batch::types::result::DispatchResult> {
        let mut results = Vec::with_capacity(tasks.len());

        // Получаем буфер для обработки
        let buffer_index = 0; // Временная заглушка

        for task in tasks {
            let result = self.crypto_processor.process_single_encryption(
                None,
                0, // sequence
                0, // packet_type
                &task.data,
                [0u8; 32], // key_material
                buffer_index,
            );

            let dispatch_result = crate::core::protocol::phantom_crypto::batch::types::result::DispatchResult {
                task_id: task.task_id,
                session_id: task.session_id.clone(),
                result: match result {
                    Ok(encrypted_data) => Ok(encrypted_data),
                    Err(e) => Err(format!("Encryption error: {}", e)),
                },
                processing_time: task.received_at.elapsed(),
                worker_id,
                priority: task.priority,
            };

            results.push(dispatch_result);
        }

        results
    }

    /// Обработка батча plaintext
    async fn process_processing_batch(
        &self,
        worker_id: usize,
        tasks: &[DispatchTask],
    ) -> Vec<crate::core::protocol::phantom_crypto::batch::types::result::DispatchResult> {
        let mut results = Vec::with_capacity(tasks.len());

        // Обрабатываем задачи
        for task in tasks {
            let dispatch_result = crate::core::protocol::phantom_crypto::batch::types::result::DispatchResult {
                task_id: task.task_id,
                session_id: task.session_id.clone(),
                result: Ok(task.data.clone()), // Просто возвращаем данные как есть
                processing_time: task.received_at.elapsed(),
                worker_id,
                priority: task.priority,
            };

            results.push(dispatch_result);

            // Логируем обработку
            debug!("Worker #{} processed task {} for session {}",
               worker_id, task.task_id, hex::encode(&task.session_id));
        }

        results
    }

    /// Обработка батча heartbeat
    async fn process_heartbeat_batch(
        &self,
        worker_id: usize,
        tasks: &[DispatchTask],
    ) -> Vec<crate::core::protocol::phantom_crypto::batch::types::result::DispatchResult> {
        let mut results = Vec::with_capacity(tasks.len());

        // Heartbeat задачи обрабатываются быстро
        for task in tasks {
            // Простая обработка heartbeat
            let result = Ok(b"Heartbeat acknowledged".to_vec());

            let dispatch_result = crate::core::protocol::phantom_crypto::batch::types::result::DispatchResult {
                task_id: task.task_id,
                session_id: task.session_id.clone(),
                result,
                processing_time: task.received_at.elapsed(),
                worker_id,
                priority: task.priority,
            };

            results.push(dispatch_result);
        }

        results
    }

    /// Отправка результата обработки
    async fn send_result(&self, result: crate::core::protocol::phantom_crypto::batch::types::result::DispatchResult) {
        if let Err(e) = self.result_tx.send(result).await {
            error!("Failed to send dispatch result: {}", e);
        }
    }

    /// Запуск обработчика результатов
    async fn start_result_handler(&self) {
        let dispatcher = self.clone();

        tokio::spawn(async move {
            let mut result_rx = dispatcher.result_rx.lock().await;

            while let Some(result) = result_rx.recv().await {
                dispatcher.handle_dispatch_result(result).await;
            }
        });
    }

    /// Обработка результата диспетчеризации
    async fn handle_dispatch_result(&self, result: crate::core::protocol::phantom_crypto::batch::types::result::DispatchResult) {
        // Отправляем результат через batch_writer если нужно
        if let Ok(response_data) = &result.result {
            if !response_data.is_empty() {
                // TODO: Определить destination_addr для ответа
                // Временная заглушка - пропускаем запись
                debug!("Would send response for task {}, but destination_addr not implemented", result.task_id);
            }
        }

        // Освобождаем backpressure permit
        self.backpressure_semaphore.add_permits(1);

        // Удаляем задачу из регистра
        {
            let mut registry = self.task_registry.write().await;
            registry.remove(&result.task_id);
        }

        // Обновляем статистику
        {
            let mut stats = self.stats.lock().await;
            stats.task_processed(result.processing_time);
        }

        trace!("Task {} processed in {:?}", result.task_id, result.processing_time);
    }

    /// Запуск балансировщика нагрузки
    async fn start_load_balancer(&self) {
        let dispatcher = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(dispatcher.config.load_balancing_interval);

            loop {
                interval.tick().await;
                dispatcher.balance_load().await;
            }
        });
    }

    /// Балансировка нагрузки между worker-ами
    async fn balance_load(&self) {
        let worker_states = self.worker_states.read().await;

        if worker_states.len() < 2 {
            return;
        }

        // Находим наиболее и наименее загруженных worker-ов
        let mut loads: Vec<_> = worker_states.values()
            .map(|state| (state.worker_id, state.load_factor))
            .collect();

        loads.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

        if let Some((least_loaded_id, least_load)) = loads.first() {
            if let Some((most_loaded_id, most_load)) = loads.last() {
                // Если разница в нагрузке значительная
                if most_load - least_load > 0.3 && *most_load > 0.7 {
                    debug!("Load balancing: stealing work from worker {} to {}",
                           most_loaded_id, least_loaded_id);

                    self.steal_work(*most_loaded_id, *least_loaded_id).await;
                }
            }
        }
    }

    /// Work stealing между worker-ами
    async fn steal_work(&self, from_worker_id: usize, to_worker_id: usize) {
        if !self.work_stealing_enabled {
            return;
        }

        let steal_attempt = self.steal_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if steal_attempt >= self.config.max_steal_attempts {
            return;
        }

        // Получаем задачи из очереди from_worker
        let mut queues = self.priority_queues.write().await;

        // Явно указываем тип при создании вектора
        let mut stolen_tasks: Vec<DispatchTask> = Vec::new();

        // Крадем задачи из всех очередей
        for queue in queues.iter_mut() {
            // Создаем временный вектор для задач, которые нужно украсть из этой очереди
            let mut tasks_to_steal: Vec<usize> = Vec::new(); // Здесь мы храним индексы

            // Сначала находим индексы задач для кражи
            for (i, task) in queue.iter().enumerate() {
                if stolen_tasks.len() >= 10 { // Крадем максимум 10 задач за раз
                    break;
                }

                // Крадем только не критические задачи
                if !task.priority.is_critical() {
                    tasks_to_steal.push(i);
                    stolen_tasks.push(task.clone()); // Клонируем задачу
                }
            }

            // Теперь удаляем задачи из очереди (в обратном порядке, чтобы индексы не сдвигались)
            for &index in tasks_to_steal.iter().rev() {
                if index < queue.len() {
                    queue.remove(index);
                }
            }

            if stolen_tasks.len() >= 10 {
                break;
            }
        }

        if !stolen_tasks.is_empty() {
            debug!("Stole {} tasks from worker {} to {}",
               stolen_tasks.len(), from_worker_id, to_worker_id);

            // Отправляем украденные задачи целевому worker-у
            if let Some(worker_handle) = self.workers.read().await.get(&to_worker_id) {
                for task in stolen_tasks {
                    if let Err(e) = worker_handle.send_task(task).await {
                        error!("Failed to send stolen task: {}", e);
                    }
                }
            }

            // Обновляем статистику
            {
                let mut stats = self.stats.lock().await;
                stats.work_stealing_event();
            }
        }
    }

    /// Проверка необходимости аварийного сброса
    async fn should_emergency_flush(&self) -> bool {
        let stats = self.stats.lock().await;
        stats.total_tasks_received - stats.total_tasks_processed >
            self.config.emergency_flush_threshold as u64
    }

    /// Аварийный сброс задач
    async fn emergency_flush(&self) {
        warn!("⚠️ Emergency flush triggered!");

        // Принудительно обрабатываем все задачи в очередях
        let mut queues = self.priority_queues.write().await;

        for queue in queues.iter_mut() {
            while let Some(task) = queue.pop_front() {
                // Помечаем задачу как сброшенную
                debug!("Emergency flush: dropping task {}", task.task_id);

                // Освобождаем permit
                self.backpressure_semaphore.add_permits(1);
            }
        }

        // Логируем аварийный сброс
        error!("Emergency flush triggered due to queue overflow");
    }

    /// Обновление состояния worker-а
    async fn update_worker_state(&self, worker_id: usize, tasks_processed: usize, processing_time: Duration) {
        let mut worker_states = self.worker_states.write().await;

        if let Some(state) = worker_states.get_mut(&worker_id) {
            state.increment_processed(tasks_processed);

            // Расчет load factor
            let load = tasks_processed as f64 / processing_time.as_secs_f64().max(0.001);
            state.update_load_factor(load);

            state.set_health(processing_time < self.config.batch_timeout);
        }
    }

    /// Обновление статистики диспетчера
    async fn update_dispatcher_stats(&self, batch_size: usize, processing_time: Duration) {
        let mut stats = self.stats.lock().await;

        stats.batch_processed(batch_size, processing_time);

        // Обновляем размеры очередей
        let queues = self.priority_queues.read().await;
        let queue_sizes: Vec<usize> = queues.iter().map(|q| q.len()).collect();
        stats.update_queue_sizes(queue_sizes);

        // Обновляем загрузку worker-ов
        let worker_states = self.worker_states.read().await;
        let worker_loads: Vec<f64> = worker_states.values()
            .map(|state| state.load_factor)
            .collect();
        stats.update_worker_loads(worker_loads);
    }

    /// Обновление статистики батча
    async fn update_batch_stats(&self, batch_size: usize) {
        let mut stats = self.stats.lock().await;
        stats.total_tasks_received += batch_size as u64;
    }

    /// Обработка закрытия соединения
    async fn handle_connection_closed(&self, source_addr: std::net::SocketAddr) {
        // Удаляем все задачи для этого соединения
        let mut registry = self.task_registry.write().await;
        let tasks_to_remove: Vec<_> = registry.iter()
            .filter(|(_, task)| task.source_addr == source_addr)
            .map(|(task_id, _)| *task_id)
            .collect();

        for task_id in &tasks_to_remove {
            registry.remove(task_id);
            // Освобождаем permit
            self.backpressure_semaphore.add_permits(1);
        }

        debug!("Cleaned up {} tasks for closed connection {}",
               tasks_to_remove.len(), source_addr);
    }

    /// Обработка ошибки чтения
    async fn handle_read_error(&self, source_addr: std::net::SocketAddr, error: String) {
        // Логируем ошибку
        error!("Read error from {}: {}", source_addr, error);

        // Обрабатываем как закрытие соединения
        self.handle_connection_closed(source_addr).await;
    }

    /// Получение статистики диспетчера
    pub async fn get_stats(&self) -> DispatcherStats {
        self.stats.lock().await.clone()
    }

    /// Остановка диспетчера
    pub async fn shutdown(&mut self) {
        info!("Shutting down PacketBatchDispatcher...");

        // Уведомляем все worker-ы о shutdown
        self.shutdown_notify.notify_waiters();

        // Останавливаем все worker-ы
        let mut workers = self.workers.write().await;
        for (worker_id, worker_handle) in workers.drain() {
            worker_handle.abort();
            info!("Worker #{} stopped", worker_id);
        }

        // Аварийный сброс оставшихся задач
        self.emergency_flush().await;

        info!("PacketBatchDispatcher shutdown complete");
    }

    /// Получение количества активных worker-ов
    pub async fn active_worker_count(&self) -> usize {
        let worker_states = self.worker_states.read().await;
        worker_states.values()
            .filter(|state| state.is_healthy)
            .count()
    }

    /// Получение количества задач в очередях
    pub async fn queued_task_count(&self) -> usize {
        let queues = self.priority_queues.read().await;
        queues.iter().map(|q| q.len()).sum()
    }
}

impl Clone for PacketBatchDispatcher {
    fn clone(&self) -> Self {
        let (task_tx, task_rx) = mpsc::channel(self.config.max_queue_size);
        let (result_tx, result_rx) = mpsc::channel(1000);

        Self {
            config: self.config.clone(),
            crypto_processor: self.crypto_processor.clone(),
            batch_writer: self.batch_writer.clone(),
            monitor: self.monitor.clone(),
            priority_queues: Arc::new(RwLock::new(Vec::new())),
            task_registry: Arc::new(RwLock::new(HashMap::new())),
            workers: Arc::new(RwLock::new(HashMap::new())),
            worker_states: Arc::new(RwLock::new(HashMap::new())),
            task_tx,
            task_rx: Mutex::new(task_rx),
            result_tx,
            result_rx: Mutex::new(result_rx),
            shutdown_notify: Arc::new(Notify::new()),
            backpressure_semaphore: Arc::new(Semaphore::new(self.config.max_queue_size)),
            stats: Mutex::new(DispatcherStats::new()),
            task_counter: std::sync::atomic::AtomicU64::new(0),
            batch_counter: std::sync::atomic::AtomicU64::new(0),
            work_stealing_enabled: self.work_stealing_enabled,
            steal_attempts: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}