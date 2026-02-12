use std::time::{Duration};
use std::collections::VecDeque;
use lazy_static::lazy_static;

/// Единая система приоритетов для всей batch системы
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    Critical = 0,    // Heartbeat, Ping, управляющие команды
    High = 1,        // Важные данные
    Normal = 2,      // Обычный трафик
    Low = 3,         // Фоновые задачи
    Background = 4,  // Несрочные операции
}

impl Priority {
    /// Количество уровней приоритета
    pub const LEVELS: usize = 5;

    /// Получить приоритет из байта данных
    pub fn from_byte(data: &[u8]) -> Self {
        if data.is_empty() {
            return Priority::Normal;
        }

        match data[0] {
            0x01 | 0x10 => Priority::Critical,    // Ping и Heartbeat
            _ if data.len() <= 64 => Priority::High,
            _ if data.len() > 1024 => Priority::Low,
            _ => Priority::Normal,
        }
    }

    /// Является ли приоритет критическим
    pub fn is_critical(&self) -> bool {
        matches!(self, Priority::Critical)
    }

    /// Получить из числа
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Priority::Critical),
            1 => Some(Priority::High),
            2 => Some(Priority::Normal),
            3 => Some(Priority::Low),
            4 => Some(Priority::Background),
            _ => None,
        }
    }
}

/// Функция полезности для приоритета
#[derive(Debug, Clone)]
pub struct UtilityFunction {
    /// Вес полезности
    pub weight: f64,
    /// Коэффициент задержки
    pub latency_factor: f64,
    /// Коэффициент пропускной способности
    pub throughput_factor: f64,
    /// Коэффициент потерь
    pub loss_factor: f64,
    /// Максимальная допустимая задержка
    pub max_latency: Duration,
    /// Минимальная требуемая пропускная способность
    pub min_throughput: f64,
}

impl UtilityFunction {
    pub fn new(priority: Priority) -> Self {
        match priority {
            Priority::Critical => Self {
                weight: 4.0,
                latency_factor: 0.7,
                throughput_factor: 0.2,
                loss_factor: 0.1,
                max_latency: Duration::from_millis(10),
                min_throughput: 1000.0,
            },
            Priority::High => Self {
                weight: 2.0,
                latency_factor: 0.5,
                throughput_factor: 0.3,
                loss_factor: 0.2,
                max_latency: Duration::from_millis(50),
                min_throughput: 500.0,
            },
            Priority::Normal => Self {
                weight: 1.0,
                latency_factor: 0.4,
                throughput_factor: 0.4,
                loss_factor: 0.2,
                max_latency: Duration::from_millis(100),
                min_throughput: 200.0,
            },
            Priority::Low => Self {
                weight: 0.5,
                latency_factor: 0.3,
                throughput_factor: 0.4,
                loss_factor: 0.3,
                max_latency: Duration::from_millis(200),
                min_throughput: 100.0,
            },
            Priority::Background => Self {
                weight: 0.25,
                latency_factor: 0.2,
                throughput_factor: 0.3,
                loss_factor: 0.5,
                max_latency: Duration::from_millis(500),
                min_throughput: 50.0,
            },
        }
    }

    /// Расчёт полезности на основе задержки
    pub fn latency_utility(&self, latency: Duration) -> f64 {
        let ratio = latency.as_millis() as f64 / self.max_latency.as_millis() as f64;
        if ratio <= 1.0 {
            1.0 - ratio * 0.5
        } else {
            0.5 / ratio
        }
    }

    /// Расчёт полезности на основе пропускной способности
    pub fn throughput_utility(&self, throughput: f64) -> f64 {
        if throughput >= self.min_throughput {
            1.0
        } else {
            throughput / self.min_throughput
        }
    }

    /// Расчёт полезности на основе вероятности потерь
    pub fn loss_utility(&self, loss_probability: f64) -> f64 {
        1.0 - loss_probability.clamp(0.0, 1.0)
    }

    /// Общая полезность
    pub fn total_utility(&self, latency: Duration, throughput: f64, loss_probability: f64) -> f64 {
        let u_lat = self.latency_utility(latency);
        let u_thr = self.throughput_utility(throughput);
        let u_loss = self.loss_utility(loss_probability);

        self.latency_factor * u_lat +
            self.throughput_factor * u_thr +
            self.loss_factor * u_loss
    }
}

/// Сброс цвета
pub const ANSI_RESET: &str = "\x1b[0m";

#[derive(Debug, Clone)]
pub struct PriorityCalibrator {
    /// Исторические данные о времени обработки
    pub processing_times: [VecDeque<Duration>; 5],

    /// Исторические данные о пропускной способности
    pub throughputs: [VecDeque<f64>; 5],

    /// Исторические данные о вероятности потерь
    pub loss_probabilities: [VecDeque<f64>; 5],

    /// Текущие веса приоритетов
    pub current_weights: [f64; 5],

    /// Целевые полезности
    pub target_utilities: [f64; 5],

    /// Максимальный размер истории
    pub max_history: usize,
}

impl PriorityCalibrator {
    pub fn new(max_history: usize) -> Self {
        Self {
            processing_times: [
                VecDeque::with_capacity(max_history),
                VecDeque::with_capacity(max_history),
                VecDeque::with_capacity(max_history),
                VecDeque::with_capacity(max_history),
                VecDeque::with_capacity(max_history),
            ],
            throughputs: [
                VecDeque::with_capacity(max_history),
                VecDeque::with_capacity(max_history),
                VecDeque::with_capacity(max_history),
                VecDeque::with_capacity(max_history),
                VecDeque::with_capacity(max_history),
            ],
            loss_probabilities: [
                VecDeque::with_capacity(max_history),
                VecDeque::with_capacity(max_history),
                VecDeque::with_capacity(max_history),
                VecDeque::with_capacity(max_history),
                VecDeque::with_capacity(max_history),
            ],
            current_weights: [4.0, 2.0, 1.0, 0.5, 0.25],
            target_utilities: [0.95, 0.9, 0.85, 0.8, 0.75],
            max_history,
        }
    }

    /// Расчёт средней полезности для приоритета
    pub fn average_utility(&self, priority: Priority) -> f64 {
        let idx = priority as usize;
        let utility_fn = UtilityFunction::new(priority);

        let avg_latency = self.processing_times[idx]
            .iter()
            .sum::<Duration>() / self.processing_times[idx].len().max(1) as u32;

        let avg_throughput = self.throughputs[idx]
            .iter()
            .sum::<f64>() / self.throughputs[idx].len().max(1) as f64;

        let avg_loss = self.loss_probabilities[idx]
            .iter()
            .sum::<f64>() / self.loss_probabilities[idx].len().max(1) as f64;

        utility_fn.total_utility(avg_latency, avg_throughput, avg_loss)
    }
}

lazy_static! {
    static ref PRIORITY_CALIBRATOR: std::sync::RwLock<PriorityCalibrator> =
        std::sync::RwLock::new(PriorityCalibrator::new(1000));
}