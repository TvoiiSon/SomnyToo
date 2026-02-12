use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use lazy_static::lazy_static;
use crate::core::protocol::batch_system::types::priority::Priority;

/// Веса приоритетов на основе частоты и критичности
#[derive(Debug, Clone)]
pub struct PacketClassificationModel {
    /// Частота появления каждого типа пакета
    pub frequency: HashMap<u8, f64>,

    /// Критичность каждого типа пакета (0-1)
    pub criticality: HashMap<u8, f64>,

    /// Среднее время обработки
    pub avg_processing_time: HashMap<u8, Duration>,

    /// Средний размер пакета
    pub avg_size: HashMap<u8, usize>,

    /// Временные ряды для прогнозирования
    pub time_series: HashMap<u8, VecDeque<Instant>>,

    /// Максимальная история
    pub max_history: usize,
}

impl PacketClassificationModel {
    pub fn new(max_history: usize) -> Self {
        Self {
            frequency: HashMap::new(),
            criticality: HashMap::new(),
            avg_processing_time: HashMap::new(),
            avg_size: HashMap::new(),
            time_series: HashMap::new(),
            max_history,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketType {
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

    /// Вес приоритета для QoS
    pub fn priority_weight(&self) -> f64 {
        match self {
            PacketType::Ping => 4.0,
            PacketType::Heartbeat => 3.0,
        }
    }

    /// Требует ли пакет немедленной отправки (flush)
    pub fn requires_immediate_flush(&self) -> bool {
        matches!(self, PacketType::Ping | PacketType::Heartbeat)
    }

    /// Базовая критичность (0-1)
    pub fn base_criticality(&self) -> f64 {
        match self {
            PacketType::Ping => 0.9,
            PacketType::Heartbeat => 0.8,
        }
    }

    /// Типичный размер пакета (байт)
    pub fn typical_size(&self) -> usize {
        match self {
            PacketType::Ping => 64,
            PacketType::Heartbeat => 32,
        }
    }

    /// Максимальное допустимое время обработки
    pub fn max_processing_time(&self) -> Duration {
        match self {
            PacketType::Ping => Duration::from_millis(10),
            PacketType::Heartbeat => Duration::from_millis(50),
        }
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

#[derive(Debug, Clone)]
pub struct PacketInfo {
    pub packet_type: PacketType,
    pub priority: Priority,
    pub priority_weight: f64,
    pub requires_flush: bool,
    pub description: &'static str,
    pub typical_size: usize,
    pub max_processing_time: Duration,
    pub base_criticality: f64,
}

#[derive(Debug, Clone)]
pub struct PacketStatistics {
    pub total_packets: u64,
    pub packets_by_type: HashMap<u8, u64>,
    pub bytes_by_type: HashMap<u8, u64>,
    pub avg_size_by_type: HashMap<u8, f64>,
    pub p95_size_by_type: HashMap<u8, f64>,
    pub p99_size_by_type: HashMap<u8, f64>,
    pub frequency_by_type: HashMap<u8, f64>,
    pub criticality_by_type: HashMap<u8, f64>,
}

impl Default for PacketStatistics {
    fn default() -> Self {
        Self {
            total_packets: 0,
            packets_by_type: HashMap::new(),
            bytes_by_type: HashMap::new(),
            avg_size_by_type: HashMap::new(),
            p95_size_by_type: HashMap::new(),
            p99_size_by_type: HashMap::new(),
            frequency_by_type: HashMap::new(),
            criticality_by_type: HashMap::new(),
        }
    }
}

lazy_static! {
    static ref SUPPORTED_PACKETS: HashMap<u8, PacketInfo> = {
        let mut map = HashMap::new();
        for packet_type in PacketType::all_supported() {
            map.insert(packet_type as u8, PacketInfo {
                packet_type,
                priority: packet_type.priority(),
                priority_weight: packet_type.priority_weight(),
                requires_flush: packet_type.requires_immediate_flush(),
                description: packet_type.description(),
                typical_size: packet_type.typical_size(),
                max_processing_time: packet_type.max_processing_time(),
                base_criticality: packet_type.base_criticality(),
            });
        }
        map
    };

    static ref CLASSIFICATION_MODEL: std::sync::RwLock<PacketClassificationModel> =
        std::sync::RwLock::new(PacketClassificationModel::new(1000));

    static ref PACKET_STATS: std::sync::RwLock<PacketStatistics> =
        std::sync::RwLock::new(PacketStatistics::default());
}

/// Проверка, поддерживается ли тип пакета
pub fn is_packet_supported(byte: u8) -> bool {
    SUPPORTED_PACKETS.contains_key(&byte)
}

/// Получить приоритет для пакета
pub fn get_packet_priority(byte: u8) -> Option<Priority> {
    SUPPORTED_PACKETS.get(&byte).map(|info| info.priority)
}