use std::collections::HashMap;

/// Приоритет пакета
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Normal,
    Urgent,
}

/// Таблица приоритетов для конкретных кодов пакетов
fn build_priority_map() -> HashMap<u8, Priority> {
    let mut m = HashMap::new();
    m.insert(0x01, Priority::Urgent); // Ping
    m.insert(0x02, Priority::Urgent); // ActivateLicense
    m.insert(0x03, Priority::Urgent); // GetLicenseStatus
    m.insert(0x04, Priority::Urgent); // DeactivationLicense
    m.insert(0x05, Priority::Normal); // CreateCommunity
    m.insert(0x06, Priority::Normal); // GetAllCommunities
    m.insert(0x07, Priority::Normal); // GetInformationCommunity
    m.insert(0x08, Priority::Normal); // JoinCommunity
    m.insert(0x09, Priority::Normal); // UnsubscribeCommunity
    m.insert(0x10, Priority::Urgent); // Heartbeat
    m.insert(0x11, Priority::Normal); // GetUserSubscriptions
    m.insert(0x12, Priority::Normal); // CreateRecord
    m.insert(0x13, Priority::Normal); // GetRecords
    m.insert(0x14, Priority::Normal); // UpdateRecord
    m.insert(0x15, Priority::Normal); // DeleteRecord
    m.insert(0x16, Priority::Normal); // GetRecordById
    m.insert(0x17, Priority::Normal); // SearchRecords
    m.insert(0x18, Priority::Normal); // HashtagOperations
    m.insert(0x19, Priority::Normal); // GetHashtagStats
    m
}

/// Функция для определения приоритета пакета
pub fn determine_priority(frame: &[u8]) -> Priority {
    if frame.len() < 5 {
        return Priority::Normal;
    }

    // Проверяем magic байты (0xAB, 0xCD)
    const HEADER_MAGIC: [u8; 2] = [0xAB, 0xCD];
    if frame.len() < 2 || &frame[0..2] != HEADER_MAGIC {
        return Priority::Normal;
    }

    let ptype_raw = frame[4];
    let map = build_priority_map();
    map.get(&ptype_raw).copied().unwrap_or(Priority::Normal)
}