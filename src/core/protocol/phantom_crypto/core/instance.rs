use std::sync::Arc;
use crate::core::protocol::phantom_crypto::core::keys::PhantomSession;
use crate::core::protocol::error::{ProtocolResult};

pub struct PhantomCrypto {
    // Поля структуры
}

impl PhantomCrypto {
    pub fn new() -> Self {
        Self {
            // Инициализация
        }
    }

    pub async fn process_packet(
        &self,
        session: Arc<PhantomSession>,
        data: Vec<u8>,
    ) -> ProtocolResult<(u8, Vec<u8>)> {
        // Используем PhantomPacketProcessor
        use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;

        let processor = PhantomPacketProcessor::new();
        processor.process_incoming(&data, &session)
    }

    pub async fn encrypt(
        &self,
        session: Arc<PhantomSession>,
        packet_type: u8,
        data: Vec<u8>,
    ) -> ProtocolResult<Vec<u8>> {
        // Используем PhantomPacketProcessor
        use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;

        let processor = PhantomPacketProcessor::new();
        processor.create_outgoing(&session, packet_type, &data)
    }
}

impl Default for PhantomCrypto {
    fn default() -> Self {
        Self::new()
    }
}