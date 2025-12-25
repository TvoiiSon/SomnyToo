#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketPriority {
    HandshakeCritical,    // Высший приоритет
    SmallHighPriority,    // Мелкие пакеты ≤200b
    DataTransferMedium,   // Средние пакеты 201-1000b
    LargeBackground,      // Крупные пакеты >1000b
    HeartbeatLow,         // Фоновый приоритет
}

pub struct PacketClassifier {
    pub small_threshold: usize,
    pub large_threshold: usize,
    pub max_packet_size: usize,
}

impl Default for PacketClassifier {
    fn default() -> Self {
        Self {
            small_threshold: 200,
            large_threshold: 1000,
            max_packet_size: 4096,
        }
    }
}

impl PacketClassifier {
    pub fn classify(&self, packet_data: &[u8]) -> PacketPriority {
        let size = packet_data.len();

        if size > self.max_packet_size {
            return PacketPriority::LargeBackground;
        }

        match size {
            0..=200 => {
                if Self::is_handshake_packet(packet_data) {
                    PacketPriority::HandshakeCritical
                } else if Self::is_heartbeat_packet(packet_data) {
                    PacketPriority::HeartbeatLow
                } else {
                    PacketPriority::SmallHighPriority
                }
            },
            201..=1000 => PacketPriority::DataTransferMedium,
            _ => PacketPriority::LargeBackground,
        }
    }

    fn is_handshake_packet(data: &[u8]) -> bool {
        !data.is_empty() && data[0] == 0x01
    }

    fn is_heartbeat_packet(data: &[u8]) -> bool {
        !data.is_empty() && data[0] == 0x02
    }
}