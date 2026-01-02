use async_trait::async_trait;
use tracing::info;
use std::sync::Arc;

use super::common::{PipelineStage, PipelineContext, StageError};
use crate::core::protocol::crypto::crypto_pool_phantom::PhantomCryptoPool;

pub struct PhantomEncryptionStage {
    response_packet_type: u8,
    crypto_pool: Arc<PhantomCryptoPool>,
}

impl PhantomEncryptionStage {
    pub fn new(response_packet_type: u8, crypto_pool: Arc<PhantomCryptoPool>) -> Self {
        Self { response_packet_type, crypto_pool }
    }
}

#[async_trait]
impl PipelineStage for PhantomEncryptionStage {
    async fn execute(&self, context: &mut PipelineContext) -> Result<(), StageError> {
        let processed_data = context.processed_data
            .take()
            .ok_or_else(|| StageError::EncryptionFailed("No processed data available".to_string()))?;

        info!("Encrypting phantom response of {} bytes", processed_data.len());

        let encrypted_response = self.crypto_pool.encrypt(
            context.phantom_session.clone(),
            self.response_packet_type,
            processed_data,
        ).await
            .map_err(|e| StageError::EncryptionFailed(e.to_string()))?;

        context.encrypted_response = Some(encrypted_response);
        Ok(())
    }
}