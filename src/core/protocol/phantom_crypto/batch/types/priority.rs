/// Приоритет батча
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BatchPriority {
    Realtime = 0,    // Heartbeat, управляющие пакеты
    High = 1,        // Важные данные
    Normal = 2,      // Обычный трафик
    Low = 3,         // Фоновая синхронизация
    Background = 4,  // Несрочные операции
}