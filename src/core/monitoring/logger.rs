use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::{EnvFilter};

// Кастомный формат времени
struct CustomTime;

impl FormatTime for CustomTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"))
    }
}

pub fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // ИСПРАВЛЕНИЕ: убираем ненужную переменную и используем прямую цепочку вызовов
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_timer(CustomTime)
        .with_ansi(true)
        .with_target(true)
        .with_level(true)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_file(true)    // Добавляем файл
        .with_line_number(true) // Добавляем номер строки
        .init();
}

// Простые макросы для мониторинга
#[macro_export]
macro_rules! monitor_info {
    ($msg:literal $(, $key:ident = $value:expr)*) => {
        tracing::info!(
            monitor_component = module_path!(),
            $($key = $value,)*
            $msg
        )
    };
    ($($arg:tt)*) => {
        tracing::info!(
            monitor_component = module_path!(),
            $($arg)*
        )
    };
}

#[macro_export]
macro_rules! monitor_error {
    ($msg:literal $(, $key:ident = $value:expr)*) => {
        tracing::error!(
            monitor_component = module_path!(),
            $($key = $value,)*
            $msg
        )
    };
    ($($arg:tt)*) => {
        tracing::error!(
            monitor_component = module_path!(),
            $($arg)*
        )
    };
}

#[macro_export]
macro_rules! monitor_warn {
    ($msg:literal $(, $key:ident = $value:expr)*) => {
        tracing::warn!(
            monitor_component = module_path!(),
            $($key = $value,)*
            $msg
        )
    };
    ($($arg:tt)*) => {
        tracing::warn!(
            monitor_component = module_path!(),
            $($arg)*
        )
    };
}

#[macro_export]
macro_rules! monitor_debug {
    ($msg:literal $(, $key:ident = $value:expr)*) => {
        tracing::debug!(
            monitor_component = module_path!(),
            $($key = $value,)*
            $msg
        )
    };
    ($($arg:tt)*) => {
        tracing::debug!(
            monitor_component = module_path!(),
            $($arg)*
        )
    };
}