use super::parameter::QueryParameter;

pub struct InsertBuilder {
    table: String,
    columns: Vec<String>,
    values: Vec<Vec<QueryParameter>>,
    returning: Vec<String>,
    on_conflict: Option<ConflictResolution>,
}

#[derive(Debug, Clone)]
pub enum ConflictResolution {
    DoNothing,
    DoUpdate(Vec<(String, QueryParameter)>),
}

impl InsertBuilder {
    pub fn new(table: &str) -> Self {
        Self {
            table: table.to_string(),
            columns: Vec::new(),
            values: Vec::new(),
            returning: Vec::new(),
            on_conflict: None,
        }
    }

    // ✅ ИСПРАВЛЕНО: Метод теперь принимает только columns
    pub fn columns(mut self, columns: &[&str]) -> Self {
        self.columns = columns.iter().map(|s| s.to_string()).collect();
        self
    }

    // ✅ ДОБАВЛЕНО: Отдельный метод для values
    pub fn values(mut self, values: &[QueryParameter]) -> Self {
        if self.columns.len() != values.len() {
            panic!("Columns and values must have the same length");
        }
        self.values.push(values.to_vec());
        self
    }

    // ✅ ДОБАВЛЕНО: Метод для установки колонок и значений вместе (старый API)
    pub fn columns_and_values(mut self, columns: &[&str], values: &[QueryParameter]) -> Self {
        if columns.len() != values.len() {
            panic!("Columns and values must have the same length");
        }

        self.columns = columns.iter().map(|s| s.to_string()).collect();
        self.values.push(values.to_vec());
        self
    }

    pub fn multiple_values(mut self, values_list: &[Vec<QueryParameter>]) -> Self {
        for values in values_list {
            if self.columns.len() != values.len() {
                panic!("Columns and values must have the same length");
            }
            self.values.push(values.clone());
        }
        self
    }

    pub fn returning(mut self, columns: &[&str]) -> Self {
        self.returning = columns.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn on_conflict_do_nothing(mut self) -> Self {
        self.on_conflict = Some(ConflictResolution::DoNothing);
        self
    }

    pub fn on_conflict_do_update(mut self, updates: &[(&str, QueryParameter)]) -> Self {
        let update_pairs = updates.iter()
            .map(|(col, val)| (col.to_string(), val.clone()))
            .collect();
        self.on_conflict = Some(ConflictResolution::DoUpdate(update_pairs));
        self
    }

    pub fn build(&self) -> String {
        if self.columns.is_empty() {
            panic!("No columns specified for INSERT");
        }

        if self.values.is_empty() {
            panic!("No values specified for INSERT");
        }

        let columns = self.columns.join(", ");

        let values_sets: Vec<String> = self.values.iter()
            .map(|value_set| {
                let values: Vec<String> = value_set.iter()
                    .map(|param| param.to_sql())
                    .collect();
                format!("({})", values.join(", "))
            })
            .collect();

        let values_str = values_sets.join(", ");

        let on_conflict = match &self.on_conflict {
            Some(ConflictResolution::DoNothing) => {
                " ON CONFLICT DO NOTHING".to_string()
            }
            Some(ConflictResolution::DoUpdate(updates)) => {
                let update_clauses: Vec<String> = updates.iter()
                    .map(|(col, val)| format!("{} = {}", col, val.to_sql()))
                    .collect();
                format!(" ON CONFLICT DO UPDATE SET {}", update_clauses.join(", "))
            }
            None => String::new(),
        };

        let returning = if !self.returning.is_empty() {
            format!(" RETURNING {}", self.returning.join(", "))
        } else {
            String::new()
        };

        format!(
            "INSERT INTO {} ({}) VALUES {}{}{}",
            self.table, columns, values_str, on_conflict, returning
        )
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.table.is_empty() {
            return Err("Table name cannot be empty".to_string());
        }

        if self.columns.is_empty() {
            return Err("No columns specified".to_string());
        }

        if self.values.is_empty() {
            return Err("No values specified".to_string());
        }

        for (i, value_set) in self.values.iter().enumerate() {
            if value_set.len() != self.columns.len() {
                return Err(format!(
                    "Values set {} has {} values, but {} columns specified",
                    i + 1,
                    value_set.len(),
                    self.columns.len()
                ));
            }
        }

        Ok(())
    }
}