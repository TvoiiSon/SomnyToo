use once_cell::sync::Lazy;
use std::sync::Arc;
use super::micro_limiter::{HighPrecisionRateLimiter, RateLimitConfig};

// ГЛОБАЛЬНЫЙ ИНСТАНС RATE LIMITER (с более мягкими настройками)
pub static RATE_LIMITER: Lazy<Arc<HighPrecisionRateLimiter>> = Lazy::new(|| {
    let config = RateLimitConfig {
        small_packets_per_second: 1_000_000,    // Увеличил до 1M мелких/сек
        medium_packets_per_second: 500_000,     // Увеличил до 500K средних/сек
        large_packets_per_second: 100_000,      // Увеличил до 100K крупных/сек
        control_packets_per_second: 50_000,     // Увеличил до 50K control/сек
        burst_window_micros: 50_000,            // Увеличил до 50ms бёрст-окно
        sliding_window_size: 5,                 // Добавил скользящее окно
        max_identical_packets: 100,             // Максимум одинаковых пакетов в окне
    };

    Arc::new(HighPrecisionRateLimiter::new(config))
});