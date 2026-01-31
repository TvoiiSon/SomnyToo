use async_trait::async_trait;
use tracing::info;
use std::sync::Arc;

use super::common::{PipelineStage, PipelineContext, StageError};
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;

pub struct PhantomDecryptionStage {
    crypto_pool: Arc<PhantomPacketProcessor>,
}

impl PhantomDecryptionStage {
    pub fn new(crypto_pool: Arc<PhantomPacketProcessor>) -> Self {
        Self { crypto_pool }
    }
}

#[async_trait]
impl PipelineStage for PhantomDecryptionStage {
    async fn execute(&self, context: &mut PipelineContext) -> Result<(), StageError> {
        info!("Processing phantom packet for session {}",
              hex::encode(context.phantom_session.session_id()));

        // Используем PhantomPacketProcessor
        let result = self.crypto_pool.process_incoming(
            &context.raw_payload,
            &context.phantom_session
        )
            .map_err(|e| StageError::DecryptionFailed(e.to_string()))?;

        context.packet_type = Some(result.0);
        context.decrypted_data = Some(result.1);

        info!("Successfully processed phantom packet type: 0x{:02X}", result.0);
        Ok(())
    }
}