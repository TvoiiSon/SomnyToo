use async_trait::async_trait;
use tracing::info;
use std::sync::Arc;

use super::common::{PipelineStage, PipelineContext, StageError};
use crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto;

pub struct PhantomEncryptionStage {
    response_packet_type: u8,
    crypto_pool: Arc<PhantomCrypto>,
}

impl PhantomEncryptionStage {
    pub fn new(response_packet_type: u8, crypto_pool: Arc<PhantomCrypto>) -> Self {
        Self { response_packet_type, crypto_pool }
    }

    // Добавляем метод для получения response_packet_type
    pub fn response_packet_type(&self) -> u8 {
        self.response_packet_type
    }

    // Добавляем метод для получения crypto_pool
    pub fn crypto_pool(&self) -> Arc<PhantomCrypto> {
        self.crypto_pool.clone()
    }
}

#[async_trait]
impl PipelineStage for PhantomEncryptionStage {
    async fn execute(&self, context: &mut PipelineContext) -> Result<(), StageError> {
        let processed_data = context.processed_data
            .take()
            .ok_or_else(|| StageError::EncryptionFailed("No processed data available".to_string()))?;

        info!("Preparing phantom response of {} bytes, packet type: 0x{:02X}",
              processed_data.len(), self.response_packet_type);

        // Используем crypto_pool для шифрования
        let encrypted_response = self.crypto_pool.encrypt(
            context.phantom_session.clone(),
            self.response_packet_type,
            processed_data,
        ).await
            .map_err(|e| StageError::EncryptionFailed(e.to_string()))?;

        context.encrypted_response = Some(encrypted_response);

        info!("Successfully encrypted response with packet type 0x{:02X}",
              self.response_packet_type);
        Ok(())
    }
}