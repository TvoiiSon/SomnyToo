#[derive(Debug, Clone, PartialEq)]
pub enum Order {
    Asc,
    Desc,
}

#[derive(Debug, Clone)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Full,
    Cross, // ✅ ДОБАВЛЕНО: CROSS JOIN
    Natural, // ✅ ДОБАВЛЕНО: NATURAL JOIN
}

#[derive(Debug, Clone)]
pub struct Join {
    pub join_type: JoinType,
    pub table: String,
    pub on: String,
    pub alias: Option<String>, // ✅ ДОБАВЛЕНО: алиас для таблицы
}

impl Join {
    pub fn new(join_type: JoinType, table: &str, on: &str) -> Self {
        Self {
            join_type,
            table: table.to_string(),
            on: on.to_string(),
            alias: None,
        }
    }

    pub fn new_with_alias(join_type: JoinType, table: &str, alias: &str, on: &str) -> Self {
        Self {
            join_type,
            table: table.to_string(),
            on: on.to_string(),
            alias: Some(alias.to_string()),
        }
    }

    pub fn inner(table: &str, on: &str) -> Self {
        Self::new(JoinType::Inner, table, on)
    }

    pub fn inner_with_alias(table: &str, alias: &str, on: &str) -> Self {
        Self::new_with_alias(JoinType::Inner, table, alias, on)
    }

    pub fn left(table: &str, on: &str) -> Self {
        Self::new(JoinType::Left, table, on)
    }

    pub fn left_with_alias(table: &str, alias: &str, on: &str) -> Self {
        Self::new_with_alias(JoinType::Left, table, alias, on)
    }

    pub fn right(table: &str, on: &str) -> Self {
        Self::new(JoinType::Right, table, on)
    }

    pub fn right_with_alias(table: &str, alias: &str, on: &str) -> Self {
        Self::new_with_alias(JoinType::Right, table, alias, on)
    }

    pub fn full(table: &str, on: &str) -> Self {
        Self::new(JoinType::Full, table, on)
    }

    pub fn cross(table: &str) -> Self {
        Self {
            join_type: JoinType::Cross,
            table: table.to_string(),
            on: String::new(), // CROSS JOIN не имеет ON условия
            alias: None,
        }
    }

    pub fn natural(join_type: JoinType, table: &str) -> Self {
        Self {
            join_type,
            table: table.to_string(),
            on: String::new(), // NATURAL JOIN не имеет явного ON условия
            alias: None,
        }
    }

    // ✅ ДОБАВЛЕНО: Валидация JOIN
    pub fn validate(&self) -> Result<(), String> {
        if self.table.trim().is_empty() {
            return Err("Join table cannot be empty".to_string());
        }

        match self.join_type {
            JoinType::Cross | JoinType::Natural => {
                // CROSS JOIN и NATURAL JOIN не должны иметь ON условия
                if !self.on.is_empty() {
                    return Err(format!("{:?} JOIN should not have ON condition", self.join_type));
                }
            }
            _ => {
                // Остальные JOIN должны иметь ON условие
                if self.on.trim().is_empty() {
                    return Err("JOIN condition cannot be empty".to_string());
                }
            }
        }

        Ok(())
    }

    // ✅ ДОБАВЛЕНО: Получение SQL представления JOIN
    pub fn to_sql(&self) -> String {
        let join_type_str = match self.join_type {
            JoinType::Inner => "INNER JOIN",
            JoinType::Left => "LEFT JOIN",
            JoinType::Right => "RIGHT JOIN",
            JoinType::Full => "FULL JOIN",
            JoinType::Cross => "CROSS JOIN",
            JoinType::Natural => "NATURAL JOIN",
        };

        let table_expr = if let Some(alias) = &self.alias {
            format!("{} AS {}", self.table, alias)
        } else {
            self.table.clone()
        };

        match self.join_type {
            JoinType::Cross | JoinType::Natural => {
                format!(" {} {}", join_type_str, table_expr)
            }
            _ => {
                format!(" {} {} ON {}", join_type_str, table_expr, self.on)
            }
        }
    }
}

// ✅ ДОБАВЛЕНО: Структура для представления UNION
#[derive(Debug, Clone)]
pub struct Union {
    pub query: String,
    pub union_type: UnionType,
}

#[derive(Debug, Clone)]
pub enum UnionType {
    Union,
    UnionAll,
    Intersect,
    Except,
}

impl Union {
    pub fn new(query: &str, union_type: UnionType) -> Self {
        Self {
            query: query.to_string(),
            union_type,
        }
    }

    pub fn to_sql(&self) -> String {
        match self.union_type {
            UnionType::Union => format!("UNION {}", self.query),
            UnionType::UnionAll => format!("UNION ALL {}", self.query),
            UnionType::Intersect => format!("INTERSECT {}", self.query),
            UnionType::Except => format!("EXCEPT {}", self.query),
        }
    }
}

// ✅ ДОБАВЛЕНО: Структура для представления CTE (Common Table Expressions)
#[derive(Debug, Clone)]
pub struct CommonTableExpression {
    pub name: String,
    pub query: String,
    pub column_names: Vec<String>,
}

impl CommonTableExpression {
    pub fn new(name: &str, query: &str) -> Self {
        Self {
            name: name.to_string(),
            query: query.to_string(),
            column_names: Vec::new(),
        }
    }

    pub fn with_columns(mut self, columns: &[&str]) -> Self {
        self.column_names = columns.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn to_sql(&self) -> String {
        if self.column_names.is_empty() {
            format!("{} AS ({})", self.name, self.query)
        } else {
            let columns = self.column_names.join(", ");
            format!("{}({}) AS ({})", self.name, columns, self.query)
        }
    }
}