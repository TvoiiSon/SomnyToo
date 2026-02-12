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

    /// Все уровни приоритета
    pub fn all_levels() -> [Priority; Self::LEVELS] {
        [
            Priority::Critical,
            Priority::High,
            Priority::Normal,
            Priority::Low,
            Priority::Background,
        ]
    }

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

    /// Является ли приоритет высоким
    pub fn is_high(&self) -> bool {
        matches!(self, Priority::High)
    }

    /// Является ли приоритет фоновым
    pub fn is_background(&self) -> bool {
        matches!(self, Priority::Background)
    }

    /// Получить числовое значение (0-4)
    pub fn as_u8(&self) -> u8 {
        *self as u8
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

    /// Название приоритета
    pub fn name(&self) -> &'static str {
        match self {
            Priority::Critical => "Critical",
            Priority::High => "High",
            Priority::Normal => "Normal",
            Priority::Low => "Low",
            Priority::Background => "Background",
        }
    }

    /// Цвет для логирования
    pub fn log_color(&self) -> &'static str {
        match self {
            Priority::Critical => "\x1b[31m", // Красный
            Priority::High => "\x1b[33m",     // Жёлтый
            Priority::Normal => "\x1b[32m",   // Зелёный
            Priority::Low => "\x1b[36m",      // Голубой
            Priority::Background => "\x1b[90m", // Серый
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

/// Преобразование приоритета в вес для QoS (Generalized Processor Sharing)
pub fn priority_to_qos_weight(priority: Priority) -> f64 {
    match priority {
        Priority::Critical => 4.0,
        Priority::High => 2.0,
        Priority::Normal => 1.0,
        Priority::Low => 0.5,
        Priority::Background => 0.25,
    }
}

/// Преобразование приоритета в приоритетное число (чем меньше, тем выше приоритет)
pub fn priority_to_scheduling_priority(priority: Priority) -> i32 {
    match priority {
        Priority::Critical => 0,
        Priority::High => 1,
        Priority::Normal => 2,
        Priority::Low => 3,
        Priority::Background => 4,
    }
}

/// Преобразование приоритета в цвет для логирования (ANSI)
pub fn priority_to_ansi_color(priority: Priority) -> &'static str {
    match priority {
        Priority::Critical => "\x1b[31;1m", // Ярко-красный
        Priority::High => "\x1b[33;1m",     // Ярко-жёлтый
        Priority::Normal => "\x1b[32;1m",   // Ярко-зелёный
        Priority::Low => "\x1b[36;1m",      // Ярко-голубой
        Priority::Background => "\x1b[90;1m", // Ярко-серый
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

    /// Обновление калибратора новыми данными
    pub fn update(&mut self, priority: Priority,
                  proc_time: Duration,
                  throughput: f64,
                  loss_prob: f64) {
        let idx = priority as usize;

        self.processing_times[idx].push_back(proc_time);
        if self.processing_times[idx].len() > self.max_history {
            self.processing_times[idx].pop_front();
        }

        self.throughputs[idx].push_back(throughput);
        if self.throughputs[idx].len() > self.max_history {
            self.throughputs[idx].pop_front();
        }

        self.loss_probabilities[idx].push_back(loss_prob);
        if self.loss_probabilities[idx].len() > self.max_history {
            self.loss_probabilities[idx].pop_front();
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

    /// Адаптивная калибровка весов
    pub fn calibrate_weights(&mut self) {
        for i in 0..5 {
            let priority = Priority::from_u8(i as u8).unwrap();
            let current_utility = self.average_utility(priority);
            let target = self.target_utilities[i];

            if current_utility < target * 0.9 {
                // Полезность слишком низкая - увеличиваем вес
                self.current_weights[i] *= 1.1;
            } else if current_utility > target * 1.1 {
                // Полезность высокая - можно уменьшить вес
                self.current_weights[i] *= 0.95;
            }

            // Нормировка весов
            let sum: f64 = self.current_weights.iter().sum();
            for w in &mut self.current_weights {
                *w /= sum;
            }
        }
    }

    /// Получение калиброванного веса
    pub fn calibrated_weight(&self, priority: Priority) -> f64 {
        self.current_weights[priority as usize]
    }
}

lazy_static! {
    static ref PRIORITY_CALIBRATOR: std::sync::RwLock<PriorityCalibrator> =
        std::sync::RwLock::new(PriorityCalibrator::new(1000));
}