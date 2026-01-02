use async_trait::async_trait;
use tracing::info;
use std::net::SocketAddr;
use std::sync::Arc;

use super::common::{PipelineStage, PipelineContext, StageError};
use crate::core::protocol::packets::processor::packet_service::PhantomPacketService;

pub struct PhantomProcessingStage {
    packet_service: Arc<PhantomPacketService>,  // Используем PhantomPacketService
    client_ip: SocketAddr,
}

impl PhantomProcessingStage {
    pub fn new(packet_service: Arc<PhantomPacketService>, client_ip: SocketAddr) -> Self {
        Self { packet_service, client_ip }
    }
}

#[async_trait]
impl PipelineStage for PhantomProcessingStage {
    async fn execute(&self, context: &mut PipelineContext) -> Result<(), StageError> {
        let packet_type = context.packet_type
            .ok_or_else(|| StageError::ProcessingFailed("No packet type available".to_string()))?;

        let decrypted_data = context.decrypted_data
            .take()
            .ok_or_else(|| StageError::ProcessingFailed("No decrypted data available".to_string()))?;

        info!("Processing phantom packet type: 0x{:02X} from {}",
              packet_type, self.client_ip);

        let processing_result = self.packet_service
            .process_packet(
                context.phantom_session.clone(),
                packet_type,
                decrypted_data,
                self.client_ip,
            )
            .await
            .map_err(|e| StageError::ProcessingFailed(e.to_string()))?;

        context.processed_data = Some(processing_result.response);
        Ok(())
    }
}