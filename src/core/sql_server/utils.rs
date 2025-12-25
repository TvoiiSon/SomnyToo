use std::time::Duration;

// ✅ Константы для конфигурации по умолчанию
pub const DEFAULT_MAX_CONNECTIONS: u32 = 50;
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(300);
pub const DEFAULT_HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(30);
pub const DEFAULT_RATE_LIMIT: usize = 1000;

/// Проверка валидности SQL запроса (базовая проверка)
pub fn is_valid_sql(query: &str) -> bool {
    let trimmed = query.trim();
    !trimmed.is_empty() &&
        trimmed.len() <= 10 * 1024 * 1024 && // 10MB max
        has_valid_sql_structure(trimmed)
}

fn has_valid_sql_structure(query: &str) -> bool {
    let upper = query.to_uppercase();

    // Проверяем, что запрос начинается с валидного ключевого слова
    upper.starts_with("SELECT") ||
        upper.starts_with("INSERT") ||
        upper.starts_with("UPDATE") ||
        upper.starts_with("DELETE") ||
        upper.starts_with("WITH") ||
        upper.starts_with("CREATE") ||
        upper.starts_with("ALTER") ||
        upper.starts_with("DROP")
}

/// Форматирование времени выполнения для логирования
pub fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{:.2}s", duration.as_secs_f64())
    } else if duration.as_millis() > 0 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{}μs", duration.as_micros())
    }
}

/// Создание connection string для тестов
#[cfg(test)]
pub fn test_connection_string() -> String {
    "postgres://test:test@localhost/test".to_string()
}

/// Валидация имени таблицы
pub fn is_valid_table_name(name: &str) -> bool {
    !name.is_empty() &&
        name.len() <= 64 &&
        name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Валидация имени колонки
pub fn is_valid_column_name(name: &str) -> bool {
    !name.is_empty() &&
        name.len() <= 64 &&
        name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}