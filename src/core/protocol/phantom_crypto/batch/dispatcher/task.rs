use std::time::Instant;

use super::priority::DispatchPriority;

/// Тип задачи
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TaskType {
    Decryption,     // Дешифрование входящего пакета
    Encryption,     // Шифрование исходящего пакета
    Processing,     // Обработка plaintext
    Heartbeat,      // Heartbeat обработка
}

/// Задача для диспетчера
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DispatchTask {
    pub task_id: u64,
    pub session_id: Vec<u8>,
    pub data: Vec<u8>,
    pub source_addr: std::net::SocketAddr,
    pub received_at: Instant,
    pub priority: DispatchPriority,
    pub task_type: TaskType,
}

impl DispatchTask {
    /// Создание новой задачи
    pub fn new(
        task_id: u64,
        session_id: Vec<u8>,
        data: Vec<u8>,
        source_addr: std::net::SocketAddr,
        priority: DispatchPriority,
        task_type: TaskType,
    ) -> Self {
        Self {
            task_id,
            session_id,
            data,
            source_addr,
            received_at: Instant::now(),
            priority,
            task_type,
        }
    }

    /// Получение размера данных задачи
    pub fn data_size(&self) -> usize {
        self.data.len()
    }

    /// Получение возраста задачи
    pub fn age(&self) -> std::time::Duration {
        self.received_at.elapsed()
    }

    /// Проверка, истек ли таймаут задачи
    pub fn is_timed_out(&self, timeout: std::time::Duration) -> bool {
        self.age() > timeout
    }

    /// Создание задачи дешифрования
    pub fn decryption_task(
        task_id: u64,
        session_id: Vec<u8>,
        ciphertext: Vec<u8>,
        source_addr: std::net::SocketAddr,
        priority: DispatchPriority,
    ) -> Self {
        Self::new(task_id, session_id, ciphertext, source_addr, priority, TaskType::Decryption)
    }

    /// Создание задачи шифрования
    pub fn encryption_task(
        task_id: u64,
        session_id: Vec<u8>,
        plaintext: Vec<u8>,
        source_addr: std::net::SocketAddr,
        priority: DispatchPriority,
    ) -> Self {
        Self::new(task_id, session_id, plaintext, source_addr, priority, TaskType::Encryption)
    }

    /// Создание задачи обработки
    pub fn processing_task(
        task_id: u64,
        session_id: Vec<u8>,
        data: Vec<u8>,
        source_addr: std::net::SocketAddr,
        priority: DispatchPriority,
    ) -> Self {
        Self::new(task_id, session_id, data, source_addr, priority, TaskType::Processing)
    }

    /// Создание heartbeat задачи
    pub fn heartbeat_task(
        task_id: u64,
        session_id: Vec<u8>,
        data: Vec<u8>,
        source_addr: std::net::SocketAddr,
    ) -> Self {
        Self::new(task_id, session_id, data, source_addr, DispatchPriority::Critical, TaskType::Heartbeat)
    }
}