use std::time::Duration;

/// Результат пакетной обработки
#[derive(Debug, Clone)]
pub struct BatchResult<T = Vec<u8>> {
    pub batch_id: u64,
    pub results: Vec<Result<T, String>>,
    pub processing_time: Duration,
    pub successful: usize,
    pub failed: usize,
}

impl<T> BatchResult<T> {
    /// Создание успешного результата
    pub fn success(batch_id: u64, results: Vec<T>, processing_time: Duration) -> Self {
        let successful = results.len();
        let results = results.into_iter().map(Ok).collect();

        Self {
            batch_id,
            results,
            processing_time,
            successful,
            failed: 0,
        }
    }

    /// Создание результата с ошибками
    pub fn with_errors(
        batch_id: u64,
        results: Vec<Result<T, String>>,
        processing_time: Duration,
    ) -> Self {
        let successful = results.iter().filter(|r| r.is_ok()).count();
        let failed = results.iter().filter(|r| r.is_err()).count();

        Self {
            batch_id,
            results,
            processing_time,
            successful,
            failed,
        }
    }

    /// Проверка, успешен ли результат
    pub fn is_successful(&self) -> bool {
        self.failed == 0
    }

    /// Получение только успешных результатов
    pub fn successes(&self) -> impl Iterator<Item = &T> {
        self.results.iter().filter_map(|r| r.as_ref().ok())
    }

    /// Получение ошибок
    pub fn errors(&self) -> impl Iterator<Item = &String> {
        self.results.iter().filter_map(|r| r.as_ref().err())
    }

    /// Преобразование результатов
    pub fn map<U, F>(self, f: F) -> BatchResult<U>
    where
        F: Fn(T) -> U,
    {
        let results = self.results.into_iter()
            .map(|r| r.map(&f))
            .collect();

        BatchResult {
            batch_id: self.batch_id,
            results,
            processing_time: self.processing_time,
            successful: self.successful,
            failed: self.failed,
        }
    }

    /// Объединение результатов
    pub fn merge(mut self, other: BatchResult<T>) -> Self {
        self.results.extend(other.results);
        self.successful += other.successful;
        self.failed += other.failed;
        self.processing_time = self.processing_time.max(other.processing_time);
        self
    }

    /// Получение статистики
    pub fn stats(&self) -> BatchStats {
        BatchStats {
            batch_id: self.batch_id,
            total: self.successful + self.failed,
            successful: self.successful,
            failed: self.failed,
            processing_time: self.processing_time,
            success_rate: if self.successful + self.failed > 0 {
                self.successful as f64 / (self.successful + self.failed) as f64
            } else {
                0.0
            },
        }
    }
}

/// Статистика батча
#[derive(Debug, Clone)]
pub struct BatchStats {
    pub batch_id: u64,
    pub total: usize,
    pub successful: usize,
    pub failed: usize,
    pub processing_time: Duration,
    pub success_rate: f64,
}

impl BatchStats {
    /// Получение throughput (операций в секунду)
    pub fn throughput(&self) -> f64 {
        if self.processing_time.as_secs_f64() > 0.0 {
            self.total as f64 / self.processing_time.as_secs_f64()
        } else {
            0.0
        }
    }

    /// Форматирование для логов
    pub fn to_log_string(&self) -> String {
        format!(
            "Batch #{}: {}/{} successful ({:.1}%), time: {:?}, throughput: {:.1} ops/sec",
            self.batch_id,
            self.successful,
            self.total,
            self.success_rate * 100.0,
            self.processing_time,
            self.throughput()
        )
    }
}

/// Результат диспетчеризации
#[derive(Debug, Clone)]
pub struct DispatchResult {
    pub task_id: u64,
    pub session_id: Vec<u8>,
    pub result: Result<Vec<u8>, String>,
    pub processing_time: Duration,
    pub worker_id: usize,
    pub priority: super::priority::Priority,
}

impl DispatchResult {
    /// Проверка, успешен ли результат
    pub fn is_successful(&self) -> bool {
        self.result.is_ok()
    }

    /// Получение данных результата
    pub fn data(&self) -> Option<&[u8]> {
        self.result.as_ref().ok().map(|v| v.as_slice())
    }

    /// Получение ошибки
    pub fn error(&self) -> Option<&String> {
        self.result.as_ref().err()
    }
}

/// Результат операции ввода-вывода
#[derive(Debug, Clone)]
pub struct IOResult {
    pub bytes_processed: usize,
    pub processing_time: Duration,
    pub source_addr: std::net::SocketAddr,
    pub session_id: Vec<u8>,
}

impl IOResult {
    /// Расчет скорости передачи (байт/сек)
    pub fn bytes_per_second(&self) -> f64 {
        if self.processing_time.as_secs_f64() > 0.0 {
            self.bytes_processed as f64 / self.processing_time.as_secs_f64()
        } else {
            0.0
        }
    }

    /// Форматирование для логов
    pub fn to_log_string(&self) -> String {
        format!(
            "IO from {}: {} bytes in {:?} ({:.1} KB/s)",
            self.source_addr,
            self.bytes_processed,
            self.processing_time,
            self.bytes_per_second() / 1024.0
        )
    }
}