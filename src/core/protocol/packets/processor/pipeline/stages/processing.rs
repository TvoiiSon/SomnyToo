use async_trait::async_trait;
use tracing::info;
use std::net::SocketAddr;

use super::common::{PipelineStage, PipelineContext, StageError};
use crate::core::protocol::packets::processor::packet_service::PacketService;

pub struct ProcessingStage {
    packet_service: PacketService,
    client_ip: SocketAddr,
}

impl ProcessingStage {
    pub fn new(packet_service: PacketService, client_ip: SocketAddr) -> Self {
        Self { packet_service, client_ip }
    }
}

#[async_trait]
impl PipelineStage for ProcessingStage {
    async fn execute(&self, context: &mut PipelineContext) -> Result<(), StageError> {
        let packet_type = context.packet_type
            .ok_or_else(|| StageError::ProcessingFailed("No packet type available".to_string()))?;

        let decrypted_data = context.decrypted_data
            .take()
            .ok_or_else(|| StageError::ProcessingFailed("No decrypted data available".to_string()))?;

        info!("Processing packet type: {:?} from {}",
              crate::core::protocol::packets::decoder::packet_parser::PacketType::from(packet_type),
              self.client_ip);

        let processing_result = self.packet_service
            .process_packet(
                context.session_keys.clone(),
                crate::core::protocol::packets::decoder::packet_parser::PacketType::from(packet_type),
                decrypted_data,
                self.client_ip,
            )
            .await
            .map_err(|e| StageError::ProcessingFailed(e.to_string()))?;

        context.processed_data = Some(processing_result.response);
        Ok(())
    }
}