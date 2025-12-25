use super::condition::Condition;

pub struct DeleteBuilder {
    table: String,
    where_condition: Option<Condition>,
    returning: Vec<String>,
    using_tables: Vec<String>, // ✅ ДОБАВЛЕНО: поддержка USING
}

impl DeleteBuilder {
    pub fn new(table: &str) -> Self {
        Self {
            table: table.to_string(),
            where_condition: None,
            returning: Vec::new(),
            using_tables: Vec::new(),
        }
    }

    pub fn r#where(mut self, condition: Condition) -> Self {
        self.where_condition = Some(condition);
        self
    }

    pub fn and_where(mut self, condition: Condition) -> Self {
        // ✅ ДОБАВЛЕНО: Добавление условий через AND
        match &mut self.where_condition {
            Some(existing) => {
                self.where_condition = Some(Condition::And(vec![
                    existing.clone(),
                    condition
                ]));
            }
            None => {
                self.where_condition = Some(condition);
            }
        }
        self
    }

    pub fn returning(mut self, columns: &[&str]) -> Self {
        self.returning = columns.iter().map(|s| s.to_string()).collect();
        self
    }

    // ✅ ДОБАВЛЕНО: Поддержка USING для сложных DELETE
    pub fn using(mut self, tables: &[&str]) -> Self {
        self.using_tables = tables.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn build(&self) -> String {
        // ✅ ДОБАВЛЕНО: USING clause
        let using_clause = if !self.using_tables.is_empty() {
            format!(" USING {}", self.using_tables.join(", "))
        } else {
            String::new()
        };

        let where_clause = self.where_condition.as_ref()
            .map(|cond| format!(" WHERE {}", self.build_condition(cond)))
            .unwrap_or_default();

        let returning = if !self.returning.is_empty() {
            format!(" RETURNING {}", self.returning.join(", "))
        } else {
            String::new()
        };

        format!(
            "DELETE FROM {}{}{}{}",
            self.table, using_clause, where_clause, returning
        )
    }

    fn build_condition(&self, condition: &Condition) -> String {
        match condition {
            Condition::Simple { field, operator, value } => {
                match operator.to_string().as_str() {
                    "IS NULL" | "IS NOT NULL" => {
                        format!("{} {}", field, operator)
                    }
                    _ => {
                        format!("{} {} '{}'", field, operator, value)
                    }
                }
            }
            Condition::And(conditions) => {
                let parts: Vec<String> = conditions.iter()
                    .map(|cond| self.build_condition(cond))
                    .collect();
                if parts.len() == 1 {
                    parts[0].clone()
                } else {
                    format!("({})", parts.join(" AND "))
                }
            }
            Condition::Or(conditions) => {
                let parts: Vec<String> = conditions.iter()
                    .map(|cond| self.build_condition(cond))
                    .collect();
                if parts.len() == 1 {
                    parts[0].clone()
                } else {
                    format!("({})", parts.join(" OR "))
                }
            }
            Condition::Group(condition) => {
                format!("({})", self.build_condition(condition))
            }
            Condition::Raw(sql) => sql.clone(),
            // ✅ ИСПРАВЛЕНО: Добавлен обработчик для Parameterized
            Condition::Parameterized { field, operator, parameter_index } => {
                if matches!(operator, super::condition::Operator::IsNull | super::condition::Operator::IsNotNull) {
                    format!("{} {}", field, operator)
                } else {
                    format!("{} {} ${}", field, operator, parameter_index)
                }
            }
        }
    }

    // ✅ ДОБАВЛЕНО: Метод для проверки валидности
    pub fn validate(&self) -> Result<(), String> {
        if self.table.is_empty() {
            return Err("Table name cannot be empty".to_string());
        }

        // Предупреждение для DELETE без WHERE (но не блокируем)
        if self.where_condition.is_none() {
            eprintln!("WARNING: DELETE without WHERE condition may affect all rows");
        }

        // Проверяем, что USING используется только с WHERE
        if !self.using_tables.is_empty() && self.where_condition.is_none() {
            return Err("USING clause requires WHERE condition".to_string());
        }

        Ok(())
    }
}