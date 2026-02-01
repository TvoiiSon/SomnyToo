use std::time::Instant;
use tokio::io::AsyncRead;
use bytes::BytesMut;

/// Читатель для конкретного соединения
pub struct ConnectionReader {
    pub source_addr: std::net::SocketAddr,
    pub session_id: Vec<u8>,
    pub read_stream: Box<dyn AsyncRead + Unpin + Send + Sync>,
    pub buffer: BytesMut,
    pub last_read_time: Instant,
    pub frames_read: u64,
    pub is_active: bool,
    pub is_ready: bool,
}

impl ConnectionReader {
    /// Создание нового читателя соединения
    pub fn new(
        source_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        read_stream: Box<dyn AsyncRead + Unpin + Send + Sync>,
        buffer_capacity: usize,
    ) -> Self {
        Self {
            source_addr,
            session_id: session_id.clone(),
            read_stream,
            buffer: BytesMut::with_capacity(buffer_capacity),
            last_read_time: Instant::now(),
            frames_read: 0,
            is_active: true,
            is_ready: true,
        }
    }

    /// Сброс состояния
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.last_read_time = Instant::now();
        self.frames_read = 0;
        self.is_active = true;
    }

    /// Проверка активности соединения
    pub fn is_active(&self) -> bool {
        self.is_active && !self.session_id.is_empty()
    }

    /// Получение времени с последнего чтения
    pub fn time_since_last_read(&self) -> std::time::Duration {
        Instant::now().duration_since(self.last_read_time)
    }
}

impl Clone for ConnectionReader {
    fn clone(&self) -> Self {
        // Используем tokio::io::empty() вместо std::io::empty()
        Self {
            source_addr: self.source_addr,
            session_id: self.session_id.clone(),
            read_stream: Box::new(tokio::io::empty()),
            buffer: BytesMut::with_capacity(self.buffer.capacity()),
            last_read_time: self.last_read_time,
            frames_read: self.frames_read,
            is_active: self.is_active,
            is_ready: self.is_ready,
        }
    }
}