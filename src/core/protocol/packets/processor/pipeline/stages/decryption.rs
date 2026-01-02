use async_trait::async_trait;
use tracing::info;
use std::sync::Arc;

use super::common::{PipelineStage, PipelineContext, StageError};
use crate::core::protocol::crypto::crypto_pool_phantom::PhantomCryptoPool;

pub struct PhantomDecryptionStage {
    crypto_pool: Arc<PhantomCryptoPool>,
}

impl PhantomDecryptionStage {
    pub fn new(crypto_pool: Arc<PhantomCryptoPool>) -> Self {
        Self { crypto_pool }
    }
}

#[async_trait]
impl PipelineStage for PhantomDecryptionStage {
    async fn execute(&self, context: &mut PipelineContext) -> Result<(), StageError> {
        info!("Decrypting phantom packet for session {}",
              hex::encode(context.phantom_session.session_id()));

        let result = self.crypto_pool.decrypt(
            context.phantom_session.clone(),
            context.raw_payload.clone()
        ).await
            .map_err(|e| StageError::DecryptionFailed(e.to_string()))?;

        context.packet_type = Some(result.0);
        context.decrypted_data = Some(result.1);

        info!("Successfully decrypted phantom packet type: 0x{:02X}", result.0);
        Ok(())
    }
}