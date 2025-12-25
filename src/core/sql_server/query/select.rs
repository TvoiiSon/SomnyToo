use std::marker::PhantomData;

use super::types::{Join, Order, JoinType};
use super::condition::Condition;

pub struct SelectBuilder<T> {
    table: String,
    fields: Vec<String>,
    joins: Vec<Join>,
    where_conditions: Option<Condition>,
    order_by: Vec<(String, Order)>,
    limit: Option<u32>,
    offset: Option<u32>,
    distinct: bool,
    group_by: Vec<String>, // ✅ ДОБАВЛЕНО: группировка
    having: Option<Condition>, // ✅ ДОБАВЛЕНО: условие HAVING
    _phantom: PhantomData<T>,
}

impl<T> SelectBuilder<T> {
    pub fn new(table: &str) -> Self {
        Self {
            table: table.to_string(),
            fields: Vec::new(),
            joins: Vec::new(),
            where_conditions: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            group_by: Vec::new(),
            having: None,
            _phantom: PhantomData,
        }
    }

    pub fn fields(mut self, fields: &[&str]) -> Self {
        self.fields = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn all_fields(mut self) -> Self {
        self.fields.clear();
        self
    }

    pub fn join(mut self, join: Join) -> Self {
        self.joins.push(join);
        self
    }

    pub fn r#where(mut self, condition: Condition) -> Self {
        self.where_conditions = Some(condition);
        self
    }

    pub fn and_where(mut self, condition: Condition) -> Self {
        // ✅ ДОБАВЛЕНО: Добавление условий через AND
        match &mut self.where_conditions {
            Some(existing) => {
                self.where_conditions = Some(Condition::And(vec![
                    existing.clone(),
                    condition
                ]));
            }
            None => {
                self.where_conditions = Some(condition);
            }
        }
        self
    }

    pub fn order_by(mut self, field: &str, order: Order) -> Self {
        self.order_by.push((field.to_string(), order));
        self
    }

    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }

    // ✅ ДОБАВЛЕНО: Методы для GROUP BY и HAVING
    pub fn group_by(mut self, fields: &[&str]) -> Self {
        self.group_by = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn having(mut self, condition: Condition) -> Self {
        self.having = Some(condition);
        self
    }

    pub fn build(&self) -> String {
        let distinct = if self.distinct { "DISTINCT " } else { "" };

        let fields = if self.fields.is_empty() {
            "*".to_string()
        } else {
            self.fields.join(", ")
        };

        let joins: String = self.joins.iter()
            .map(|join| {
                let join_type = match join.join_type {
                    JoinType::Inner => "INNER JOIN",
                    JoinType::Left => "LEFT JOIN",
                    JoinType::Right => "RIGHT JOIN",
                    JoinType::Full => "FULL JOIN",
                    // ✅ ИСПРАВЛЕНО: Добавлены недостающие варианты
                    JoinType::Cross => "CROSS JOIN",
                    JoinType::Natural => "NATURAL JOIN",
                };
                format!(" {} {} ON {}", join_type, join.table, join.on)
            })
            .collect();

        let where_clause = self.where_conditions.as_ref()
            .map(|cond| format!(" WHERE {}", self.build_condition(cond)))
            .unwrap_or_default();

        // ✅ ДОБАВЛЕНО: GROUP BY clause
        let group_by = if !self.group_by.is_empty() {
            format!(" GROUP BY {}", self.group_by.join(", "))
        } else {
            String::new()
        };

        // ✅ ДОБАВЛЕНО: HAVING clause
        let having = self.having.as_ref()
            .map(|cond| format!(" HAVING {}", self.build_condition(cond)))
            .unwrap_or_default();

        let order_by = if !self.order_by.is_empty() {
            let orders: Vec<String> = self.order_by.iter()
                .map(|(field, order)| {
                    let order_str = match order {
                        Order::Asc => "ASC",
                        Order::Desc => "DESC",
                    };
                    format!("{} {}", field, order_str)
                })
                .collect();
            format!(" ORDER BY {}", orders.join(", "))
        } else {
            String::new()
        };

        let limit = self.limit.map(|l| format!(" LIMIT {}", l)).unwrap_or_default();
        let offset = self.offset.map(|o| format!(" OFFSET {}", o)).unwrap_or_default();

        format!(
            "SELECT {}{} FROM {}{}{}{}{}{}{}{}",
            distinct, fields, self.table, joins, where_clause,
            group_by, having, order_by, limit, offset
        )
    }

    fn build_condition(&self, condition: &Condition) -> String {
        match condition {
            Condition::Simple { field, operator, value } => {
                match operator.to_string().as_str() {
                    "IS NULL" | "IS NOT NULL" => {
                        format!("{} {}", field, operator)
                    }
                    "IN" => {
                        format!("{} {} {}", field, operator, value)
                    }
                    "BETWEEN" => {
                        format!("{} {} {}", field, operator, value)
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
}

impl<T> SelectBuilder<T> {
    pub fn into<U>(self) -> SelectBuilder<U> {
        SelectBuilder {
            table: self.table,
            fields: self.fields,
            joins: self.joins,
            where_conditions: self.where_conditions,
            order_by: self.order_by,
            limit: self.limit,
            offset: self.offset,
            distinct: self.distinct,
            group_by: self.group_by,
            having: self.having,
            _phantom: PhantomData,
        }
    }

    // ✅ ДОБАВЛЕНО: Метод для проверки валидности запроса
    pub fn validate(&self) -> Result<(), String> {
        if self.table.is_empty() {
            return Err("Table name cannot be empty".to_string());
        }

        // Проверяем, что LIMIT и OFFSET используются правильно
        if self.offset.is_some() && self.limit.is_none() {
            return Err("OFFSET cannot be used without LIMIT".to_string());
        }

        // Проверяем, что HAVING используется только с GROUP BY
        if self.having.is_some() && self.group_by.is_empty() {
            return Err("HAVING clause requires GROUP BY".to_string());
        }

        Ok(())
    }
}