/// Единая система приоритетов для всей batch системы
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    Critical = 0,    // Heartbeat, управляющие команды
    High = 1,        // Важные данные
    Normal = 2,      // Обычный трафик
    Low = 3,         // Фоновые задачи
    Background = 4,  // Несрочные операции
}

impl Priority {
    pub fn from_byte(data: &[u8]) -> Self {
        if data.is_empty() {
            return Priority::Normal;
        }

        match data[0] {
            0x01 | 0x10 => Priority::Critical,    // PING и Heartbeat
            _ if data.len() <= 64 => Priority::High,
            _ if data.len() > 1024 => Priority::Low,
            _ => Priority::Normal,
        }
    }

    pub fn is_critical(&self) -> bool {
        matches!(self, Priority::Critical)
    }

    pub fn is_high(&self) -> bool {
        matches!(self, Priority::Critical | Priority::High)
    }
}