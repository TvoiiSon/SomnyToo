#[derive(Debug, Clone)]
pub enum QueryParameter {
    String(String),
    Integer(i64),
    Boolean(bool),
    Null,
    Float(f64),
    Decimal(String),
    Date(String),
    DateTime(String),
    Json(String),
    Bytes(Vec<u8>),
}

impl QueryParameter {
    pub fn to_sql(&self) -> String {
        match self {
            QueryParameter::String(s) => format!("'{}'", s.replace("'", "''")),
            QueryParameter::Integer(i) => i.to_string(),
            QueryParameter::Boolean(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            QueryParameter::Null => "NULL".to_string(),
            QueryParameter::Float(f) => f.to_string(),
            QueryParameter::Decimal(d) => d.to_string(),
            QueryParameter::Date(d) => format!("DATE '{}'", d),
            QueryParameter::DateTime(dt) => format!("TIMESTAMP '{}'", dt),
            QueryParameter::Json(j) => format!("'{}'::json", j.replace("'", "''")),
            QueryParameter::Bytes(b) => {
                format!("'\\x{}'", hex::encode(b))
            }
        }
    }

    pub fn get_type(&self) -> &'static str {
        match self {
            QueryParameter::String(_) => "string",
            QueryParameter::Integer(_) => "integer",
            QueryParameter::Boolean(_) => "boolean",
            QueryParameter::Null => "null",
            QueryParameter::Float(_) => "float",
            QueryParameter::Decimal(_) => "decimal",
            QueryParameter::Date(_) => "date",
            QueryParameter::DateTime(_) => "datetime",
            QueryParameter::Json(_) => "json",
            QueryParameter::Bytes(_) => "bytes",
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            QueryParameter::String(s) => {
                if s.len() > 100_000 {
                    return Err("String parameter too long".to_string());
                }
                Ok(())
            }
            QueryParameter::Integer(i) => {
                // ✅ ИСПРАВЛЕНО: Осмысленная проверка вместо бесполезного сравнения
                // Проверяем на специальные значения которые могут вызвать проблемы
                if *i == i64::MIN || *i == i64::MAX {
                    return Err("Integer parameter at extreme bounds may cause issues".to_string());
                }

                // ✅ ИСПРАВЛЕНО: Правильная проверка на переполнение для 32-битных систем
                let i32_max = i32::MAX as i64;
                if *i > i32_max {
                    return Err("Integer too large for 32-bit systems".to_string());
                }

                // Проверяем на отрицательные значения если ожидается положительное
                if *i < 0 {
                    // Здесь можно добавить логику для проверки допустимости отрицательных значений
                    // Например, для ID они обычно недопустимы
                }

                Ok(())
            }
            QueryParameter::Float(f) => {
                if !f.is_finite() {
                    return Err("Float parameter must be finite".to_string());
                }
                Ok(())
            }
            QueryParameter::Decimal(d) => {
                // Простая проверка формата десятичного числа
                if !d.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-') {
                    return Err("Invalid decimal format".to_string());
                }
                Ok(())
            }
            QueryParameter::Date(d) => {
                // Простая проверка формата даты (можно улучшить)
                if d.len() != 10 || !d.chars().all(|c| c.is_ascii_digit() || c == '-') {
                    return Err("Invalid date format, expected YYYY-MM-DD".to_string());
                }
                Ok(())
            }
            QueryParameter::DateTime(dt) => {
                // Простая проверка формата даты-времени
                if dt.len() < 19 {
                    return Err("Invalid datetime format".to_string());
                }
                Ok(())
            }
            QueryParameter::Json(j) => {
                // Проверяем валидность JSON
                if serde_json::from_str::<serde_json::Value>(j).is_err() {
                    return Err("Invalid JSON format".to_string());
                }
                Ok(())
            }
            QueryParameter::Bytes(b) => {
                if b.len() > 10_000_000 {
                    return Err("Bytes parameter too large".to_string());
                }
                Ok(())
            }
            _ => Ok(())
        }
    }

    pub fn from_string(s: String) -> Self {
        QueryParameter::String(s)
    }

    pub fn from_integer(i: i64) -> Self {
        QueryParameter::Integer(i)
    }

    pub fn from_boolean(b: bool) -> Self {
        QueryParameter::Boolean(b)
    }

    // Конструкторы для новых типов
    pub fn from_float(f: f64) -> Self {
        QueryParameter::Float(f)
    }

    pub fn from_decimal(d: String) -> Self {
        QueryParameter::Decimal(d)
    }

    pub fn from_date(d: String) -> Self {
        QueryParameter::Date(d)
    }

    pub fn from_datetime(dt: String) -> Self {
        QueryParameter::DateTime(dt)
    }

    pub fn from_json(j: String) -> Self {
        QueryParameter::Json(j)
    }

    pub fn from_bytes(b: Vec<u8>) -> Self {
        QueryParameter::Bytes(b)
    }

    // Метод для создания параметра из любого типа, который реализует Into<QueryParameter>
    pub fn from_value<T: Into<QueryParameter>>(value: T) -> Self {
        value.into()
    }
}

impl From<&str> for QueryParameter {
    fn from(s: &str) -> Self {
        QueryParameter::String(s.to_string())
    }
}

impl From<String> for QueryParameter {
    fn from(s: String) -> Self {
        QueryParameter::String(s)
    }
}

impl From<i64> for QueryParameter {
    fn from(i: i64) -> Self {
        QueryParameter::Integer(i)
    }
}

impl From<bool> for QueryParameter {
    fn from(b: bool) -> Self {
        QueryParameter::Boolean(b)
    }
}

impl From<u64> for QueryParameter {
    fn from(u: u64) -> Self {
        QueryParameter::Integer(u as i64)
    }
}

impl From<i32> for QueryParameter {
    fn from(i: i32) -> Self {
        QueryParameter::Integer(i as i64)
    }
}

impl From<u32> for QueryParameter {
    fn from(u: u32) -> Self {
        QueryParameter::Integer(u as i64)
    }
}

// Реализации для новых типов
impl From<f64> for QueryParameter {
    fn from(f: f64) -> Self {
        QueryParameter::Float(f)
    }
}

impl From<f32> for QueryParameter {
    fn from(f: f32) -> Self {
        QueryParameter::Float(f as f64)
    }
}

// Структура для представления списка параметров
#[derive(Debug, Clone)]
pub struct ParameterList {
    parameters: Vec<QueryParameter>,
}

impl ParameterList {
    pub fn new() -> Self {
        Self {
            parameters: Vec::new(),
        }
    }

    pub fn add<T: Into<QueryParameter>>(&mut self, param: T) {
        self.parameters.push(param.into());
    }

    pub fn get(&self, index: usize) -> Option<&QueryParameter> {
        self.parameters.get(index)
    }

    pub fn len(&self) -> usize {
        self.parameters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parameters.is_empty()
    }

    pub fn validate_all(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        for (i, param) in self.parameters.iter().enumerate() {
            if let Err(err) = param.validate() {
                errors.push(format!("Parameter {}: {}", i, err));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl Default for ParameterList {
    fn default() -> Self {
        Self::new()
    }
}