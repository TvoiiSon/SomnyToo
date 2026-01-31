use std::time::Duration;
use crate::core::protocol::error::ProtocolResult;

/// Результат пакетной обработки
#[derive(Debug)]
pub struct BatchResult {
    pub batch_id: u64,
    pub results: Vec<ProtocolResult<Vec<u8>>>,
    pub processing_time: Duration,
    pub successful: usize,
    pub failed: usize,
    pub simd_utilization: f64, // % использования SIMD
}

/// Результат обработки задачи
#[derive(Debug, Clone)]
pub struct DispatchResult {
    pub task_id: u64,
    pub session_id: Vec<u8>,
    pub result: Result<Vec<u8>, String>,
    pub processing_time: Duration,
    pub worker_id: usize,
    pub priority: crate::core::protocol::phantom_crypto::batch::dispatcher::priority::DispatchPriority,
}