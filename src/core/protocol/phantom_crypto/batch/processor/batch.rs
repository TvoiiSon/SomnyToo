use std::time::Instant;
use super::operation::CryptoOperation;
use crate::core::protocol::phantom_crypto::batch::types::priority::BatchPriority;

/// Пакет криптографических операций
pub struct CryptoBatch {
    pub id: u64,
    pub operations: Vec<CryptoOperation>,
    pub created_at: Instant,
    pub priority: BatchPriority,
}

impl CryptoBatch {
    pub fn new(id: u64, capacity: usize, priority: BatchPriority) -> Self {
        Self {
            id,
            operations: Vec::with_capacity(capacity),
            created_at: Instant::now(),
            priority,
        }
    }

    pub fn add_operation(&mut self, op: CryptoOperation) {
        self.operations.push(op);
    }

    pub fn len(&self) -> usize {
        self.operations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    pub fn clear(&mut self) {
        self.operations.clear();
    }
}