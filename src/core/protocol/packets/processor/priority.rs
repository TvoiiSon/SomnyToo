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
    m.insert(0x10, Priority::Urgent); // Heartbeat
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