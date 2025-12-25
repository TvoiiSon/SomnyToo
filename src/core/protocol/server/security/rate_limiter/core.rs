use std::time::{SystemTime, UNIX_EPOCH};

// Микросекундная точность времени
#[inline(always)]
pub fn current_time_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}
