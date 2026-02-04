use std::time::{Instant, Duration};
use super::priority::Priority;

/// Состояние системы
#[derive(Debug, Clone)]
pub enum SystemState {
    Initializing,
    Running,
    Paused,
    ShuttingDown,
    Stopped,
}

impl SystemState {
    pub fn is_running(&self) -> bool {
        matches!(self, SystemState::Running)
    }

    pub fn can_accept_tasks(&self) -> bool {
        matches!(self, SystemState::Running | SystemState::Initializing)
    }
}

/// Состояние батча
#[derive(Debug, Clone)]
pub struct BatchState {
    pub id: u64,
    pub priority: Priority,
    pub size: usize,
    pub created_at: Instant,
    pub started_at: Option<Instant>,
    pub completed_at: Option<Instant>,
    pub status: BatchStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchStatus {
    Pending,
    Processing,
    Completed,
    Failed,
    Cancelled,
}

impl BatchState {
    pub fn new(id: u64, priority: Priority, size: usize) -> Self {
        Self {
            id,
            priority,
            size,
            created_at: Instant::now(),
            started_at: None,
            completed_at: None,
            status: BatchStatus::Pending,
        }
    }

    pub fn start_processing(&mut self) {
        self.started_at = Some(Instant::now());
        self.status = BatchStatus::Processing;
    }

    pub fn complete(&mut self, success: bool) {
        self.completed_at = Some(Instant::now());
        self.status = if success {
            BatchStatus::Completed
        } else {
            BatchStatus::Failed
        };
    }

    pub fn cancel(&mut self) {
        self.status = BatchStatus::Cancelled;
        self.completed_at = Some(Instant::now());
    }

    pub fn processing_time(&self) -> Option<Duration> {
        match (self.started_at, self.completed_at) {
            (Some(start), Some(end)) => Some(end.duration_since(start)),
            (Some(start), None) => Some(Instant::now().duration_since(start)),
            _ => None,
        }
    }

    pub fn total_time(&self) -> Duration {
        Instant::now().duration_since(self.created_at)
    }

    pub fn is_completed(&self) -> bool {
        matches!(self.status, BatchStatus::Completed)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self.status, BatchStatus::Failed)
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status, BatchStatus::Pending | BatchStatus::Processing)
    }
}

/// Состояние соединения
#[derive(Debug, Clone)]
pub struct ConnectionState {
    pub source_addr: std::net::SocketAddr,
    pub session_id: Vec<u8>,
    pub connected_at: Instant,
    pub last_activity: Instant,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub is_active: bool,
    pub error_count: u32,
}

impl ConnectionState {
    pub fn new(source_addr: std::net::SocketAddr, session_id: Vec<u8>) -> Self {
        let now = Instant::now();
        Self {
            source_addr,
            session_id,
            connected_at: now,
            last_activity: now,
            bytes_received: 0,
            bytes_sent: 0,
            is_active: true,
            error_count: 0,
        }
    }

    pub fn update_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    pub fn add_bytes_received(&mut self, bytes: usize) {
        self.bytes_received += bytes as u64;
        self.update_activity();
    }

    pub fn add_bytes_sent(&mut self, bytes: usize) {
        self.bytes_sent += bytes as u64;
        self.update_activity();
    }

    pub fn increment_error_count(&mut self) {
        self.error_count += 1;
        if self.error_count > 10 {
            self.is_active = false;
        }
    }

    pub fn idle_time(&self) -> Duration {
        Instant::now().duration_since(self.last_activity)
    }

    pub fn uptime(&self) -> Duration {
        Instant::now().duration_since(self.connected_at)
    }

    pub fn throughput_received(&self) -> f64 {
        let uptime_secs = self.uptime().as_secs_f64();
        if uptime_secs > 0.0 {
            self.bytes_received as f64 / uptime_secs
        } else {
            0.0
        }
    }

    pub fn throughput_sent(&self) -> f64 {
        let uptime_secs = self.uptime().as_secs_f64();
        if uptime_secs > 0.0 {
            self.bytes_sent as f64 / uptime_secs
        } else {
            0.0
        }
    }
}

/// Состояние worker-а
#[derive(Debug, Clone)]
pub struct WorkerState {
    pub worker_id: usize,
    pub total_tasks_processed: u64,
    pub total_processing_time: Duration,
    pub current_tasks: usize,
    pub last_activity: Instant,
    pub is_healthy: bool,
    pub load_factor: f64, // 0.0 - 1.0
}

impl WorkerState {
    pub fn new(worker_id: usize) -> Self {
        Self {
            worker_id,
            total_tasks_processed: 0,
            total_processing_time: Duration::default(),
            current_tasks: 0,
            last_activity: Instant::now(),
            is_healthy: true,
            load_factor: 0.0,
        }
    }

    pub fn start_task(&mut self) {
        self.current_tasks += 1;
        self.update_load_factor();
    }

    pub fn complete_task(&mut self, processing_time: Duration) {
        if self.current_tasks > 0 {
            self.current_tasks -= 1;
        }
        self.total_tasks_processed += 1;
        self.total_processing_time += processing_time;
        self.last_activity = Instant::now();
        self.update_load_factor();
    }

    pub fn update_load_factor(&mut self) {
        // Простая эвристика: если обрабатывает больше 10 задач одновременно - высокая нагрузка
        self.load_factor = (self.current_tasks as f64 / 10.0).clamp(0.0, 1.0);
    }

    pub fn avg_task_time(&self) -> Duration {
        if self.total_tasks_processed > 0 {
            self.total_processing_time / self.total_tasks_processed as u32
        } else {
            Duration::default()
        }
    }

    pub fn tasks_per_second(&self) -> f64 {
        let uptime = self.last_activity.duration_since(Instant::now());
        if uptime.as_secs_f64() > 0.0 {
            self.total_tasks_processed as f64 / uptime.as_secs_f64()
        } else {
            0.0
        }
    }

    pub fn is_overloaded(&self) -> bool {
        self.load_factor > 0.8
    }

    pub fn is_idle(&self) -> bool {
        self.current_tasks == 0 && self.idle_time() > Duration::from_secs(10)
    }

    pub fn idle_time(&self) -> Duration {
        Instant::now().duration_since(self.last_activity)
    }
}

/// Состояние системы мониторинга
#[derive(Debug, Clone)]
pub struct MonitoringState {
    pub system_state: SystemState,
    pub active_connections: usize,
    pub total_batches_processed: u64,
    pub total_bytes_processed: u64,
    pub avg_processing_time: Duration,
    pub error_rate: f64,
    pub memory_usage_mb: f64,
    pub cpu_usage_percent: f64,
    pub last_updated: Instant,
}

impl MonitoringState {
    pub fn new() -> Self {
        Self {
            system_state: SystemState::Initializing,
            active_connections: 0,
            total_batches_processed: 0,
            total_bytes_processed: 0,
            avg_processing_time: Duration::default(),
            error_rate: 0.0,
            memory_usage_mb: 0.0,
            cpu_usage_percent: 0.0,
            last_updated: Instant::now(),
        }
    }

    pub fn update(&mut self) {
        self.last_updated = Instant::now();
    }

    pub fn is_healthy(&self) -> bool {
        self.system_state.is_running() &&
            self.error_rate < 0.1 && // Меньше 10% ошибок
            self.memory_usage_mb < 1024.0 && // Меньше 1GB памяти
            self.cpu_usage_percent < 80.0 // Меньше 80% CPU
    }

    pub fn to_metrics_string(&self) -> String {
        format!(
            "State: {:?}, Connections: {}, Batches: {}, Bytes: {} MB, Avg Time: {:?}, Errors: {:.1}%, Memory: {:.1} MB, CPU: {:.1}%",
            self.system_state,
            self.active_connections,
            self.total_batches_processed,
            self.total_bytes_processed / 1024 / 1024,
            self.avg_processing_time,
            self.error_rate * 100.0,
            self.memory_usage_mb,
            self.cpu_usage_percent
        )
    }
}