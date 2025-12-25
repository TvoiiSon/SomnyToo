use async_trait::async_trait;
use tracing::info;
use crate::core::protocol::packets::encoder::packet_builder::PacketBuilder;

use super::common::{PipelineStage, PipelineContext, StageError};

pub struct EncryptionStage {
    response_packet_type: u8,
}

impl EncryptionStage {
    pub fn new(response_packet_type: u8) -> Self {
        Self { response_packet_type }
    }
}

#[async_trait]
impl PipelineStage for EncryptionStage {
    async fn execute(&self, context: &mut PipelineContext) -> Result<(), StageError> {
        let processed_data = context.processed_data
            .take()
            .ok_or_else(|| StageError::EncryptionFailed("No processed data available".to_string()))?;

        info!("Encrypting response of {} bytes", processed_data.len());

        let encrypted_response = PacketBuilder::build_encrypted_packet(
            &context.session_keys,
            self.response_packet_type,
            &processed_data,
        ).await;

        context.encrypted_response = Some(encrypted_response);
        Ok(())
    }
}