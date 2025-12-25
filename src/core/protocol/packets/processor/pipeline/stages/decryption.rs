use async_trait::async_trait;
use tracing::info;
use crate::core::protocol::packets::decoder::packet_parser::{PacketParser, PacketType};

use super::common::{PipelineStage, PipelineContext, StageError};

pub struct DecryptionStage;

#[async_trait]
impl PipelineStage for DecryptionStage {
    async fn execute(&self, context: &mut PipelineContext) -> Result<(), StageError> {
        info!("Decrypting packet for session {}", hex::encode(context.session_keys.session_id));

        let (packet_type, decrypted_data) = PacketParser::decode_packet(&context.session_keys, &context.raw_payload)
            .map_err(|e| StageError::DecryptionFailed(e.to_string()))?;

        context.packet_type = Some(packet_type);
        context.decrypted_data = Some(decrypted_data);

        info!("Successfully decrypted packet type: {:?}", PacketType::from(packet_type));
        Ok(())
    }
}