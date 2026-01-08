use std::sync::{Arc, Mutex};

/// Пул предвыделенных буферов для избежания аллокаций
pub struct BufferPool {
    buffers: Arc<Mutex<Vec<Vec<u8>>>>,
    buffer_size: usize,
}

impl BufferPool {
    pub fn new(buffer_size: usize, initial_capacity: usize) -> Self {
        let mut buffers = Vec::with_capacity(initial_capacity);
        for _ in 0..initial_capacity {
            buffers.push(vec![0u8; buffer_size]);
        }

        Self {
            buffers: Arc::new(Mutex::new(buffers)),
            buffer_size,
        }
    }

    pub fn acquire(&self) -> Option<Vec<u8>> {
        self.buffers.lock().unwrap().pop()
    }

    pub fn release(&self, mut buffer: Vec<u8>) {
        // Зануляем буфер для безопасности
        buffer.fill(0);
        buffer.truncate(self.buffer_size);

        let mut buffers = self.buffers.lock().unwrap();
        if buffers.len() < 1000 { // Максимальный размер пула
            buffers.push(buffer);
        }
    }

    pub fn with_buffer<T, F>(&self, f: F) -> T
    where
        F: FnOnce(&mut [u8]) -> T,
    {
        if let Some(mut buffer) = self.acquire() {
            let result = f(&mut buffer);
            self.release(buffer);
            result
        } else {
            // Если нет доступных буферов, создаем временный
            let mut temp_buffer = vec![0u8; self.buffer_size];
            f(&mut temp_buffer)
        }
    }
}