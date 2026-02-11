use std::collections::HashMap;
use lazy_static::lazy_static;
use crate::core::protocol::batch_system::types::priority::Priority;

/// Типы пакетов, поддерживаемые сервером
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketType {
    // 🔧 Управляющие пакеты (Critical)
    Ping = 0x01,
    Heartbeat = 0x10,
}

impl PacketType {
    /// Все поддерживаемые типы пакетов
    pub fn all_supported() -> Vec<PacketType> {
        vec![
            PacketType::Ping,
            PacketType::Heartbeat,
        ]
    }

    /// Получить приоритет для типа пакета
    pub fn priority(&self) -> Priority {
        match self {
            PacketType::Ping | PacketType::Heartbeat => Priority::Critical,
        }
    }

    /// Требует ли пакет немедленной отправки (flush)
    pub fn requires_immediate_flush(&self) -> bool {
        matches!(self, PacketType::Ping | PacketType::Heartbeat)
    }

    /// Является ли пакет критическим
    pub fn is_critical(&self) -> bool {
        self.priority() == Priority::Critical
    }

    /// Получить из байта
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(PacketType::Ping),
            0x10 => Some(PacketType::Heartbeat),
            _ => None,
        }
    }

    /// Получить описание пакета
    pub fn description(&self) -> &'static str {
        match self {
            PacketType::Ping => "Ping request",
            PacketType::Heartbeat => "Heartbeat signal",
        }
    }
}

impl std::fmt::Display for PacketType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:02x} ({})", *self as u8, self.description())
    }
}

lazy_static! {
    static ref SUPPORTED_PACKETS: HashMap<u8, PacketInfo> = {
        let mut map = HashMap::new();
        for packet_type in PacketType::all_supported() {
            map.insert(packet_type as u8, PacketInfo {
                packet_type,
                priority: packet_type.priority(),
                requires_flush: packet_type.requires_immediate_flush(),
                description: packet_type.description(),
            });
        }
        map
    };
}

/// Информация о пакете
#[derive(Debug, Clone)]
pub struct PacketInfo {
    pub packet_type: PacketType,
    pub priority: Priority,
    pub requires_flush: bool,
    pub description: &'static str,
}

/// Проверка, поддерживается ли тип пакета
pub fn is_packet_supported(byte: u8) -> bool {
    SUPPORTED_PACKETS.contains_key(&byte)
}

/// Получить информацию о пакете
pub fn get_packet_info(byte: u8) -> Option<&'static PacketInfo> {
    SUPPORTED_PACKETS.get(&byte)
}

/// Получить приоритет для пакета
pub fn get_packet_priority(byte: u8) -> Option<Priority> {
    SUPPORTED_PACKETS.get(&byte).map(|info| info.priority)
}