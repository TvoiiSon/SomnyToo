use tracing::{info, debug};

/// Упрощенная реализация телеметрии для начала
#[derive(Debug, Clone)]
pub struct ProtocolTelemetry;

impl ProtocolTelemetry {
    pub fn new() -> Self {
        Self
    }

    pub fn trace_packet_processing(
        &self,
        session_id: &[u8],
        packet_type: u8,
        data: &[u8],
    ) {
        debug!(
            session_id = hex::encode(session_id),
            packet_type = packet_type,
            packet_size = data.len(),
            "Processing packet"
        );
    }

    pub fn record_handshake_success(&self, session_id: &[u8], role: &str) {
        info!(
            session_id = hex::encode(session_id),
            role = role,
            "Handshake completed successfully"
        );
    }

    pub fn record_handshake_failure(&self, reason: &str, role: &str) {
        info!(
            reason = reason,
            role = role,
            "Handshake failed"
        );
    }
}

impl Default for ProtocolTelemetry {
    fn default() -> Self {
        Self::new()
    }
}