use std::sync::Arc;
use bytes::{Bytes, BytesMut};

use super::buffer_types::BufferType;
use super::unified_buffer_pool::UnifiedBufferPool;

/// Хендл для буфера, автоматически возвращающий его в пул при drop
pub struct BufferHandle {
    buffer: BytesMut,
    buffer_type: BufferType,
    pool: Arc<UnifiedBufferPool>,
}

impl BufferHandle {
    /// Создание нового BufferHandle (только для внутреннего использования в пуле)
    pub(crate) fn new(buffer: BytesMut, buffer_type: BufferType, pool: Arc<UnifiedBufferPool>) -> Self {
        Self {
            buffer,
            buffer_type,
            pool,
        }
    }

    // Остальные публичные методы остаются без изменений...
    pub fn buffer_mut(&mut self) -> &mut BytesMut {
        &mut self.buffer
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buffer
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn capacity(&self) -> usize {
        self.buffer.capacity()
    }

    pub fn buffer_type(&self) -> BufferType {
        self.buffer_type
    }

    pub fn freeze(self) -> Bytes {
        self.buffer.clone().freeze()
    }

    pub fn extend_from_slice(&mut self, slice: &[u8]) {
        self.buffer.extend_from_slice(slice);
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    pub fn resize(&mut self, new_len: usize, value: u8) {
        self.buffer.resize(new_len, value);
    }

    pub fn reserve(&mut self, additional: usize) {
        self.buffer.reserve(additional);
    }
}

impl Drop for BufferHandle {
    fn drop(&mut self) {
        let buffer = std::mem::take(&mut self.buffer);
        self.pool.release(buffer, self.buffer_type);
    }
}