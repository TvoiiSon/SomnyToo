use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Состояние worker-а
#[derive(Debug, Clone)]
pub struct WorkerState {
    pub worker_id: usize,
    pub queue_size: usize,
    pub processing_tasks: usize,
    pub total_processed: u64,
    pub last_activity: Instant,
    pub is_healthy: bool,
    pub load_factor: f64, // 0.0 - 1.0
}

impl WorkerState {
    /// Создание нового состояния worker-а
    pub fn new(worker_id: usize) -> Self {
        Self {
            worker_id,
            queue_size: 0,
            processing_tasks: 0,
            total_processed: 0,
            last_activity: Instant::now(),
            is_healthy: true,
            load_factor: 0.0,
        }
    }

    /// Обновление времени активности
    pub fn update_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Увеличение счетчика обработанных задач
    pub fn increment_processed(&mut self, count: usize) {
        self.total_processed += count as u64;
        self.update_activity();
    }

    /// Обновление размера очереди
    pub fn update_queue_size(&mut self, size: usize) {
        self.queue_size = size;
    }

    /// Обновление количества обрабатываемых задач
    pub fn update_processing_tasks(&mut self, count: usize) {
        self.processing_tasks = count;
    }

    /// Обновление коэффициента нагрузки
    pub fn update_load_factor(&mut self, load: f64) {
        self.load_factor = load.clamp(0.0, 1.0);
    }

    /// Установка состояния здоровья
    pub fn set_health(&mut self, healthy: bool) {
        self.is_healthy = healthy;
    }

    /// Проверка, активен ли worker
    pub fn is_active(&self, timeout: std::time::Duration) -> bool {
        self.is_healthy && self.last_activity.elapsed() < timeout
    }

    /// Получение общего времени бездействия
    pub fn idle_time(&self) -> std::time::Duration {
        self.last_activity.elapsed()
    }
}

/// Handle для worker-а
pub struct WorkerHandle {
    pub worker_id: usize,
    pub join_handle: JoinHandle<()>,
    pub task_tx: mpsc::Sender<crate::core::protocol::phantom_crypto::batch::dispatcher::task::DispatchTask>,
    pub shutdown_notify: std::sync::Arc<tokio::sync::Notify>,
}

impl WorkerHandle {
    /// Создание нового handle worker-а
    pub fn new(
        worker_id: usize,
        join_handle: JoinHandle<()>,
        task_tx: mpsc::Sender<crate::core::protocol::phantom_crypto::batch::dispatcher::task::DispatchTask>,
        shutdown_notify: std::sync::Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            worker_id,
            join_handle,
            task_tx,
            shutdown_notify,
        }
    }

    /// Отправка задачи worker-у
    pub async fn send_task(&self, task: crate::core::protocol::phantom_crypto::batch::dispatcher::task::DispatchTask) -> Result<(), tokio::sync::mpsc::error::SendError<crate::core::protocol::phantom_crypto::batch::dispatcher::task::DispatchTask>> {
        self.task_tx.send(task).await
    }

    /// Получение идентификатора worker-а
    pub fn worker_id(&self) -> usize {
        self.worker_id
    }

    /// Остановка worker-а
    pub fn shutdown(&self) {
        self.shutdown_notify.notify_one();
    }

    /// Принудительная остановка worker-а
    pub fn abort(&self) {
        self.join_handle.abort();
    }

    /// Проверка, завершен ли worker
    pub fn is_finished(&self) -> bool {
        self.join_handle.is_finished()
    }
}