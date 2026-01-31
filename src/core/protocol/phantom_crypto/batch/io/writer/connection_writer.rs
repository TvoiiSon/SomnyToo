use std::time::Instant;
use std::collections::VecDeque;
use tokio::io::AsyncWrite;
use bytes::BytesMut;

use super::batch_writer::WriteTask;

/// Писатель для конкретного соединения
pub struct ConnectionWriter {
    pub destination_addr: std::net::SocketAddr,
    pub session_id: Vec<u8>,
    pub write_stream: Box<dyn AsyncWrite + Unpin + Send + Sync>,
    pub write_buffer: BytesMut,
    pub last_write_time: Instant,
    pub bytes_written: u64,
    pub is_active: bool,
    pub pending_writes: VecDeque<WriteTask>,
    pub buffer_size: usize,
}

impl ConnectionWriter {
    /// Создание нового писателя соединения
    pub fn new(
        destination_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        write_stream: Box<dyn AsyncWrite + Unpin + Send + Sync>,
        buffer_capacity: usize,
    ) -> Self {
        Self {
            destination_addr,
            session_id: session_id.clone(),
            write_stream,
            write_buffer: BytesMut::with_capacity(buffer_capacity),
            last_write_time: Instant::now(),
            bytes_written: 0,
            is_active: true,
            pending_writes: VecDeque::new(),
            buffer_size: 0,
        }
    }

    /// Добавление задачи записи в очередь
    pub fn add_write_task(&mut self, task: WriteTask) {
        let data_len = task.data.len(); // Сохраняем размер до перемещения
        self.pending_writes.push_back(task);
        self.buffer_size += data_len; // Используем сохраненный размер
    }

    /// Получение следующей задачи записи
    pub fn next_write_task(&mut self) -> Option<WriteTask> {
        self.pending_writes.pop_front().map(|task| {
            self.buffer_size = self.buffer_size.saturating_sub(task.data.len());
            task
        })
    }

    /// Проверка, есть ли задачи в очереди
    pub fn has_pending_writes(&self) -> bool {
        !self.pending_writes.is_empty()
    }

    /// Получение количества ожидающих задач
    pub fn pending_writes_count(&self) -> usize {
        self.pending_writes.len()
    }

    /// Сброс состояния
    pub fn reset(&mut self) {
        self.write_buffer.clear();
        self.pending_writes.clear();
        self.buffer_size = 0;
        self.last_write_time = Instant::now();
        self.bytes_written = 0;
        self.is_active = true;
    }

    /// Проверка активности соединения
    pub fn is_active(&self) -> bool {
        self.is_active && !self.session_id.is_empty()
    }

    /// Получение времени с последней записи
    pub fn time_since_last_write(&self) -> std::time::Duration {
        Instant::now().duration_since(self.last_write_time)
    }

    /// Получение текущего размера буфера
    pub fn current_buffer_size(&self) -> usize {
        self.buffer_size
    }
}