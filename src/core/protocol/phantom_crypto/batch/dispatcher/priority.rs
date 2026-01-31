use crate::core::protocol::phantom_crypto::batch::types::priority::BatchPriority;
use crate::core::protocol::phantom_crypto::batch::io::reader::batch_reader::FramePriority;
use crate::core::protocol::phantom_crypto::batch::io::writer::batch_writer::WritePriority;

/// Приоритет диспетчеризации (более детальный чем BatchPriority)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DispatchPriority {
    Critical = 0,    // Heartbeat, keep-alive
    RealTime = 1,    // Команды реального времени
    Interactive = 2, // Интерактивные запросы
    BulkHigh = 3,    // Важные bulk операции
    BulkNormal = 4,  // Обычные bulk операции
    Background = 5,  // Фоновые задачи
    Maintenance = 6, // Обслуживание
}

impl From<FramePriority> for DispatchPriority {
    fn from(frame_priority: FramePriority) -> Self {
        match frame_priority {
            FramePriority::Critical => DispatchPriority::Critical,
            FramePriority::High => DispatchPriority::RealTime,
            FramePriority::Normal => DispatchPriority::Interactive,
            FramePriority::Low => DispatchPriority::Background,
        }
    }
}

impl From<BatchPriority> for DispatchPriority {
    fn from(batch_priority: BatchPriority) -> Self {
        match batch_priority {
            BatchPriority::Realtime => DispatchPriority::Critical,
            BatchPriority::High => DispatchPriority::RealTime,
            BatchPriority::Normal => DispatchPriority::Interactive,
            BatchPriority::Low => DispatchPriority::BulkNormal,
            BatchPriority::Background => DispatchPriority::Background,
        }
    }
}

impl From<DispatchPriority> for BatchPriority {
    fn from(priority: DispatchPriority) -> Self {
        match priority {
            DispatchPriority::Critical => BatchPriority::Realtime,
            DispatchPriority::RealTime => BatchPriority::High,
            DispatchPriority::Interactive => BatchPriority::Normal,
            DispatchPriority::BulkHigh => BatchPriority::Normal,
            DispatchPriority::BulkNormal => BatchPriority::Low,
            DispatchPriority::Background => BatchPriority::Background,
            DispatchPriority::Maintenance => BatchPriority::Background,
        }
    }
}

impl From<DispatchPriority> for WritePriority {
    fn from(priority: DispatchPriority) -> Self {
        match priority {
            DispatchPriority::Critical => WritePriority::Immediate,
            DispatchPriority::RealTime => WritePriority::High,
            DispatchPriority::Interactive => WritePriority::High,
            DispatchPriority::BulkHigh => WritePriority::Normal,
            DispatchPriority::BulkNormal => WritePriority::Normal,
            DispatchPriority::Background => WritePriority::Low,
            DispatchPriority::Maintenance => WritePriority::Low,
        }
    }
}

impl DispatchPriority {
    /// Получение числового значения приоритета
    pub fn value(&self) -> u8 {
        *self as u8
    }

    /// Проверка, является ли приоритет критическим
    pub fn is_critical(&self) -> bool {
        matches!(self, DispatchPriority::Critical)
    }

    /// Проверка, является ли приоритет высоким
    pub fn is_high_priority(&self) -> bool {
        matches!(self, DispatchPriority::Critical | DispatchPriority::RealTime | DispatchPriority::Interactive)
    }

    /// Проверка, является ли приоритет фоновым
    pub fn is_background(&self) -> bool {
        matches!(self, DispatchPriority::Background | DispatchPriority::Maintenance)
    }

    /// Получение всех приоритетов в порядке убывания важности
    pub fn all_priorities() -> Vec<Self> {
        vec![
            DispatchPriority::Critical,
            DispatchPriority::RealTime,
            DispatchPriority::Interactive,
            DispatchPriority::BulkHigh,
            DispatchPriority::BulkNormal,
            DispatchPriority::Background,
            DispatchPriority::Maintenance,
        ]
    }
}