use std::time::Instant;
use tracing::warn;

use super::assembler::KeyAssemblerFactory;

/// Время выполнения в тактах процессора
#[derive(Debug, Clone, Copy)]
pub struct ExecutionCycles {
    pub min: u64,
    pub max: u64,
    pub avg: u64,
    pub last: u64,
}

/// Статистика выполнения
#[derive(Clone)]
pub struct RuntimeStats {
    pub total_operations: u64,
    pub failed_operations: u64,
    pub timing_anomalies: u64,
    pub cycles: ExecutionCycles,
    pub avg_execution_time_ns: u64,
}

/// Исполнительный движок фантомной системы
pub struct PhantomRuntime {
    assembler_factory: KeyAssemblerFactory,
    stats: std::sync::Mutex<RuntimeStats>,
    config: RuntimeConfig,
}

#[derive(Clone)]
pub struct RuntimeConfig {
    pub max_cycles_per_operation: u64,
    pub min_cycles_per_operation: u64,
    pub enable_timing_protection: bool,
    pub enable_hardware_acceleration: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_cycles_per_operation: 1000,
            min_cycles_per_operation: 10,
            enable_timing_protection: true,
            enable_hardware_acceleration: true,
        }
    }
}

impl PhantomRuntime {
    pub fn new() -> Self {
        Self {
            assembler_factory: KeyAssemblerFactory::default(),
            stats: std::sync::Mutex::new(RuntimeStats {
                total_operations: 0,
                failed_operations: 0,
                timing_anomalies: 0,
                cycles: ExecutionCycles {
                    min: u64::MAX,
                    max: 0,
                    avg: 0,
                    last: 0,
                },
                avg_execution_time_ns: 0,
            }),
            config: RuntimeConfig::default(),
        }
    }

    /// Создает сборщик ключей, оптимизированный для текущей платформы
    pub fn create_assembler(&self) -> Box<dyn super::assembler::KeyAssembler> {
        self.assembler_factory.create_assembler()
    }

    /// Выполняет операцию с защитой от timing attacks
    pub fn execute_with_timing_protection<F, T>(&self, operation: F) -> Result<T, String>
    where
        F: FnOnce() -> T,
    {
        let start_instant = Instant::now();

        #[cfg(target_arch = "x86_64")]
        let start_cycles = unsafe { std::arch::x86_64::_rdtsc() };

        #[cfg(not(target_arch = "x86_64"))]
        let start_cycles = 0;

        // Выполняем операцию
        let result = operation();

        #[cfg(target_arch = "x86_64")]
        let end_cycles = unsafe { std::arch::x86_64::_rdtsc() };

        #[cfg(not(target_arch = "x86_64"))]
        let end_cycles = 0;

        let elapsed_time = start_instant.elapsed();
        let cycles = end_cycles.wrapping_sub(start_cycles);

        // Обновляем статистику
        self.update_stats(cycles, elapsed_time);

        // Проверяем аномалии времени выполнения
        if self.config.enable_timing_protection {
            self.check_timing_anomaly(cycles, elapsed_time)?;
        }

        Ok(result)
    }

    /// Обновляет статистику выполнения
    fn update_stats(&self, cycles: u64, elapsed_time: std::time::Duration) {
        let mut stats = self.stats.lock().unwrap();

        stats.total_operations += 1;
        stats.cycles.last = cycles;

        if cycles < stats.cycles.min {
            stats.cycles.min = cycles;
        }
        if cycles > stats.cycles.max {
            stats.cycles.max = cycles;
        }

        // Обновляем среднее количество циклов
        let total = stats.total_operations;
        stats.cycles.avg = ((stats.cycles.avg * (total - 1)) + cycles) / total;

        // Обновляем среднее время выполнения
        let elapsed_ns = elapsed_time.as_nanos() as u64;
        stats.avg_execution_time_ns = ((stats.avg_execution_time_ns * (total - 1)) + elapsed_ns) / total;

        // Проверяем на аномалии
        if cycles > self.config.max_cycles_per_operation {
            warn!(
                "Timing anomaly detected: {} cycles (max: {}), time: {:?}",
                cycles, self.config.max_cycles_per_operation, elapsed_time
            );
            stats.timing_anomalies += 1;
        }
    }

    /// Проверяет аномалии времени выполнения
    fn check_timing_anomaly(&self, cycles: u64, elapsed_time: std::time::Duration) -> Result<(), String> {
        if cycles > self.config.max_cycles_per_operation {
            return Err(format!(
                "Timing attack detected: operation took {} cycles (max: {}) in {:?}",
                cycles, self.config.max_cycles_per_operation, elapsed_time
            ));
        }

        if cycles < self.config.min_cycles_per_operation {
            return Err(format!(
                "Suspiciously fast operation: {} cycles (min: {}) in {:?}",
                cycles, self.config.min_cycles_per_operation, elapsed_time
            ));
        }

        // Проверка времени в наносекундах
        let elapsed_ns = elapsed_time.as_nanos() as u64;
        if elapsed_ns > 1_000_000 { // 1 ms
            warn!("Slow operation: {} ns", elapsed_ns);
        }

        Ok(())
    }

    /// Возвращает статистику выполнения
    pub fn get_stats(&self) -> RuntimeStats {
        self.stats.lock().unwrap().clone()
    }

    /// Обнаруживает возможности CPU
    pub fn detect_cpu_capabilities(&self) -> CpuCapabilities {
        let mut caps = CpuCapabilities::default();

        #[cfg(target_arch = "x86_64")]
        {
            caps.aes_ni = is_x86_feature_detected!("aes");
            caps.avx2 = is_x86_feature_detected!("avx2");
            caps.avx512 = is_x86_feature_detected!("avx512f");
            caps.sha_ni = is_x86_feature_detected!("sha");
        }

        #[cfg(target_arch = "aarch64")]
        {
            caps.neon = is_aarch64_feature_detected!("neon");
            // Для ARM используем отдельную проверку crypto
            caps.crypto = cfg!(target_feature = "crypto");
        }

        caps
    }
}

/// Возможности CPU
#[derive(Debug, Clone)]
pub struct CpuCapabilities {
    pub aes_ni: bool,
    pub avx2: bool,
    pub avx512: bool,
    pub sha_ni: bool,
    pub neon: bool,
    pub crypto: bool,
}

impl Default for CpuCapabilities {
    fn default() -> Self {
        Self {
            aes_ni: false,
            avx2: false,
            avx512: false,
            sha_ni: false,
            neon: false,
            crypto: false,
        }
    }
}

impl Default for PhantomRuntime {
    fn default() -> Self {
        Self::new()
    }
}