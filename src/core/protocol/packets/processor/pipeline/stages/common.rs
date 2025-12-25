use async_trait::async_trait;
use std::sync::Arc;
use crate::core::protocol::crypto::key_manager::session_keys::SessionKeys;

pub struct PipelineContext {
    pub session_keys: Arc<SessionKeys>,
    pub raw_payload: Vec<u8>,
    pub packet_type: Option<u8>,
    pub decrypted_data: Option<Vec<u8>>,
    pub processed_data: Option<Vec<u8>>,
    pub encrypted_response: Option<Vec<u8>>,
}

impl PipelineContext {
    pub fn new(session_keys: Arc<SessionKeys>, raw_payload: Vec<u8>) -> Self {
        Self {
            session_keys,
            raw_payload,
            packet_type: None,
            decrypted_data: None,
            processed_data: None,
            encrypted_response: None,
        }
    }
}

#[async_trait]
pub trait PipelineStage: Send + Sync {
    async fn execute(&self, context: &mut PipelineContext) -> Result<(), StageError>;
}

#[derive(Debug)]
pub enum StageError {
    DecryptionFailed(String),
    ProcessingFailed(String),
    EncryptionFailed(String),
}

impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StageError::DecryptionFailed(msg) => write!(f, "Decryption failed: {}", msg),
            StageError::ProcessingFailed(msg) => write!(f, "Processing failed: {}", msg),
            StageError::EncryptionFailed(msg) => write!(f, "Encryption failed: {}", msg),
        }
    }
}

impl std::error::Error for StageError {}