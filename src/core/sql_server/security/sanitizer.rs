use super::error::SecurityError;
use regex::Regex;

pub struct QuerySanitizer {
    max_query_length: usize,
    allowed_tables: Vec<String>,
    dangerous_patterns: Vec<Regex>, // ✅ ДОБАВЛЕНО: паттерны для проверки
}

impl QuerySanitizer {
    pub fn new(max_query_length: usize, allowed_tables: Vec<String>) -> Self {
        let dangerous_patterns = vec![
            Regex::new(r"(?i)(\bDROP\b|\bTRUNCATE\b|\bSHUTDOWN\b)").unwrap(),
            Regex::new(r"(?i)(\bINSERT\s+INTO\b.*\bVALUES\b)").unwrap(), // Базовые паттерны
        ];

        Self {
            max_query_length,
            allowed_tables,
            dangerous_patterns,
        }
    }

    pub fn sanitize(&self, query: &str) -> Result<(), SecurityError> {
        if query.len() > self.max_query_length {
            return Err(SecurityError::QueryTooLong);
        }

        // ✅ ДОБАВЛЕНО: Проверка опасных паттернов
        for pattern in &self.dangerous_patterns {
            if pattern.is_match(query) {
                return Err(SecurityError::SqlInjectionAttempt);
            }
        }

        // ✅ УЛУЧШЕНО: Более точная проверка разрешенных таблиц
        if !self.allowed_tables.is_empty() {
            let query_upper = query.to_uppercase();
            let mut table_found = false;

            for table in &self.allowed_tables {
                let table_upper = table.to_uppercase();

                // Проверяем различные контексты использования таблиц
                if query_upper.contains(&format!(" FROM {} ", table_upper)) ||
                    query_upper.contains(&format!(" JOIN {} ", table_upper)) ||
                    query_upper.contains(&format!(" INTO {} ", table_upper)) ||
                    query_upper.contains(&format!(" UPDATE {} ", table_upper)) ||
                    query_upper.contains(&format!(" TABLE {}", table_upper)) {
                    table_found = true;
                    break;
                }
            }

            if !table_found && self.is_data_modification_query(&query_upper) {
                return Err(SecurityError::TableNotAllowed);
            }
        }

        // ✅ ДОБАВЛЕНО: Проверка баланса скобок
        if !self.has_balanced_parentheses(query) {
            return Err(SecurityError::InvalidQueryPattern);
        }

        Ok(())
    }

    fn is_data_modification_query(&self, query: &str) -> bool {
        query.starts_with("INSERT") ||
            query.starts_with("UPDATE") ||
            query.starts_with("DELETE") ||
            query.starts_with("CREATE") ||
            query.starts_with("ALTER") ||
            query.starts_with("DROP")
    }

    fn has_balanced_parentheses(&self, query: &str) -> bool {
        let mut balance = 0;
        for ch in query.chars() {
            match ch {
                '(' => balance += 1,
                ')' => balance -= 1,
                _ => {}
            }
            if balance < 0 {
                return false;
            }
        }
        balance == 0
    }

    // ✅ ДОБАВЛЕНО: Метод для получения информации о санитайзере
    pub fn get_info(&self) -> SanitizerInfo {
        SanitizerInfo {
            max_query_length: self.max_query_length,
            allowed_tables_count: self.allowed_tables.len(),
            dangerous_patterns_count: self.dangerous_patterns.len(),
        }
    }

    // ✅ ДОБАВЛЕНО: Метод для добавления разрешенной таблицы
    pub fn add_allowed_table(&mut self, table: String) {
        self.allowed_tables.push(table);
    }

    // ✅ ДОБАВЛЕНО: Метод для удаления разрешенной таблицы
    pub fn remove_allowed_table(&mut self, table: &str) {
        self.allowed_tables.retain(|t| t != table);
    }
}

// ✅ ДОБАВЛЕНО: Информация о санитайзере
pub struct SanitizerInfo {
    pub max_query_length: usize,
    pub allowed_tables_count: usize,
    pub dangerous_patterns_count: usize,
}