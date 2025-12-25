#[derive(Debug, Clone)]
pub enum Operator {
    Eq, Ne, Gt, Ge, Lt, Le,
    Like, ILike, In, Between,
    IsNull, IsNotNull,
    NotIn, // ✅ ДОБАВЛЕНО: NOT IN
    NotLike, // ✅ ДОБАВЛЕНО: NOT LIKE
}

impl std::fmt::Display for Operator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Operator::Eq => write!(f, "="),
            Operator::Ne => write!(f, "!="),
            Operator::Gt => write!(f, ">"),
            Operator::Ge => write!(f, ">="),
            Operator::Lt => write!(f, "<"),
            Operator::Le => write!(f, "<="),
            Operator::Like => write!(f, "LIKE"),
            Operator::ILike => write!(f, "ILIKE"),
            Operator::In => write!(f, "IN"),
            Operator::Between => write!(f, "BETWEEN"),
            Operator::IsNull => write!(f, "IS NULL"),
            Operator::IsNotNull => write!(f, "IS NOT NULL"),
            Operator::NotIn => write!(f, "NOT IN"), // ✅ ДОБАВЛЕНО
            Operator::NotLike => write!(f, "NOT LIKE"), // ✅ ДОБАВЛЕНО
        }
    }
}

#[derive(Debug, Clone)]
pub enum Condition {
    Simple {
        field: String,
        operator: Operator,
        value: String,
    },
    And(Vec<Condition>),
    Or(Vec<Condition>),
    Group(Box<Condition>),
    Raw(String),
    // ✅ ДОБАВЛЕНО: Условия с параметрами для безопасного построения запросов
    Parameterized {
        field: String,
        operator: Operator,
        parameter_index: usize,
    },
}

impl Condition {
    pub fn eq<T: ToString>(field: &str, value: T) -> Self {
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::Eq,
            value: value.to_string(),
        }
    }

    pub fn ne<T: ToString>(field: &str, value: T) -> Self {
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::Ne,
            value: value.to_string(),
        }
    }

    pub fn gt<T: ToString>(field: &str, value: T) -> Self {
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::Gt,
            value: value.to_string(),
        }
    }

    pub fn ge<T: ToString>(field: &str, value: T) -> Self {
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::Ge,
            value: value.to_string(),
        }
    }

    pub fn lt<T: ToString>(field: &str, value: T) -> Self {
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::Lt,
            value: value.to_string(),
        }
    }

    pub fn le<T: ToString>(field: &str, value: T) -> Self {
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::Le,
            value: value.to_string(),
        }
    }

    pub fn like(field: &str, pattern: &str) -> Self {
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::Like,
            value: pattern.to_string(),
        }
    }

    pub fn ilike(field: &str, pattern: &str) -> Self {
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::ILike,
            value: pattern.to_string(),
        }
    }

    pub fn not_like(field: &str, pattern: &str) -> Self {
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::NotLike,
            value: pattern.to_string(),
        }
    }

    pub fn is_null(field: &str) -> Self {
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::IsNull,
            value: String::new(),
        }
    }

    pub fn is_not_null(field: &str) -> Self {
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::IsNotNull,
            value: String::new(),
        }
    }

    pub fn r#in<T: ToString>(field: &str, values: &[T]) -> Self {
        let values_str = values.iter()
            .map(|v| v.to_string())
            .collect::<Vec<String>>()
            .join(", ");
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::In,
            value: format!("({})", values_str),
        }
    }

    pub fn not_in<T: ToString>(field: &str, values: &[T]) -> Self {
        let values_str = values.iter()
            .map(|v| v.to_string())
            .collect::<Vec<String>>()
            .join(", ");
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::NotIn,
            value: format!("({})", values_str),
        }
    }

    pub fn between<T: ToString>(field: &str, from: T, to: T) -> Self {
        Condition::Simple {
            field: field.to_string(),
            operator: Operator::Between,
            value: format!("{} AND {}", from.to_string(), to.to_string()),
        }
    }

    // ✅ ДОБАВЛЕНО: Параметризованные условия для безопасного SQL
    pub fn eq_param(field: &str, param_index: usize) -> Self {
        Condition::Parameterized {
            field: field.to_string(),
            operator: Operator::Eq,
            parameter_index: param_index,
        }
    }

    pub fn like_param(field: &str, param_index: usize) -> Self {
        Condition::Parameterized {
            field: field.to_string(),
            operator: Operator::Like,
            parameter_index: param_index,
        }
    }

    pub fn and(conditions: Vec<Condition>) -> Self {
        Condition::And(conditions)
    }

    pub fn or(conditions: Vec<Condition>) -> Self {
        Condition::Or(conditions)
    }

    pub fn group(condition: Condition) -> Self {
        Condition::Group(Box::new(condition))
    }

    pub fn raw(sql: &str) -> Self {
        Condition::Raw(sql.to_string())
    }

    // ✅ ДОБАВЛЕНО: Метод для получения всех полей из условий
    pub fn get_fields(&self) -> Vec<String> {
        match self {
            Condition::Simple { field, .. } => vec![field.clone()],
            Condition::And(conditions) | Condition::Or(conditions) => {
                conditions.iter().flat_map(|c| c.get_fields()).collect()
            }
            Condition::Group(condition) => condition.get_fields(),
            Condition::Raw(_) => Vec::new(),
            Condition::Parameterized { field, .. } => vec![field.clone()],
        }
    }

    // ✅ ДОБАВЛЕНО: Метод для проверки валидности условий
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Condition::Simple { field, operator, value } => {
                if field.trim().is_empty() {
                    return Err("Field name cannot be empty".to_string());
                }

                // Проверяем специальные операторы
                match operator {
                    Operator::IsNull | Operator::IsNotNull => {
                        if !value.is_empty() {
                            return Err(format!("Operator {} should not have a value", operator));
                        }
                    }
                    Operator::In | Operator::NotIn => {
                        if !value.starts_with('(') || !value.ends_with(')') {
                            return Err("IN operator requires values in parentheses".to_string());
                        }
                    }
                    _ => {}
                }

                Ok(())
            }
            Condition::And(conditions) | Condition::Or(conditions) => {
                if conditions.is_empty() {
                    return Err("AND/OR conditions cannot be empty".to_string());
                }
                for condition in conditions {
                    condition.validate()?;
                }
                Ok(())
            }
            Condition::Group(condition) => condition.validate(),
            Condition::Raw(sql) => {
                if sql.trim().is_empty() {
                    return Err("Raw SQL cannot be empty".to_string());
                }
                Ok(())
            }
            Condition::Parameterized { field, parameter_index, .. } => {
                if field.trim().is_empty() {
                    return Err("Field name cannot be empty".to_string());
                }
                if *parameter_index == 0 {
                    return Err("Parameter index must be greater than 0".to_string());
                }
                Ok(())
            }
        }
    }
}

impl std::fmt::Display for Condition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Condition::Simple { field, operator, value } => {
                if matches!(operator, Operator::IsNull | Operator::IsNotNull) {
                    write!(f, "{} {}", field, operator)
                } else {
                    write!(f, "{} {} {}", field, operator, value)
                }
            }
            Condition::And(conditions) => {
                let parts: Vec<String> = conditions.iter().map(|c| c.to_string()).collect();
                write!(f, "({})", parts.join(" AND "))
            }
            Condition::Or(conditions) => {
                let parts: Vec<String> = conditions.iter().map(|c| c.to_string()).collect();
                write!(f, "({})", parts.join(" OR "))
            }
            Condition::Group(condition) => {
                write!(f, "({})", condition)
            }
            Condition::Raw(sql) => {
                write!(f, "{}", sql)
            }
            Condition::Parameterized { field, operator, parameter_index } => {
                if matches!(operator, Operator::IsNull | Operator::IsNotNull) {
                    write!(f, "{} {}", field, operator)
                } else {
                    write!(f, "{} {} ${}", field, operator, parameter_index)
                }
            }
        }
    }
}