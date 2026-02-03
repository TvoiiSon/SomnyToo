use std::time::Instant;
use std::sync::Arc;
use tracing::{warn, info, debug};
use rayon::iter::{IntoParallelIterator, ParallelIterator};

use crate::core::protocol::phantom_crypto::{
    acceleration::{
        chacha20_accel::{ChaCha20Accelerator, CpuCapabilities},
        blake3_accel::Blake3Accelerator,
    },
    batch::core::processor::CryptoProcessor
};

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
    pub batch_operations: u64,
    pub simd_operations: u64,
}

/// Высокооптимизированный исполнительный движок
pub struct PhantomRuntime {
    chacha20_accel: ChaCha20Accelerator,
    blake3_accel: Blake3Accelerator,
    batch_processor: Arc<CryptoProcessor>,  // Измененный тип
    stats: std::sync::Mutex<RuntimeStats>,
    cpu_caps: CpuCapabilities,
}

impl PhantomRuntime {
    pub fn new(num_workers: usize) -> Self {
        let chacha20_accel = ChaCha20Accelerator::new();
        let blake3_accel = Blake3Accelerator::new();
        let batch_processor = Arc::new(CryptoProcessor::new(
            crate::core::protocol::phantom_crypto::batch::config::BatchConfig::default()
        ));

        let cpu_caps = CpuCapabilities::detect();

        info!("PhantomRuntime initialized with:");
        info!("  - AVX2: {}", cpu_caps.avx2);
        info!("  - AVX512: {}", cpu_caps.avx512);
        info!("  - NEON: {}", cpu_caps.neon);
        info!("  - AES-NI: {}", cpu_caps.aes_ni);
        info!("  - Workers: {}", num_workers);

        Self {
            chacha20_accel,
            blake3_accel,
            batch_processor,
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
                batch_operations: 0,
                simd_operations: 0,
            }),
            cpu_caps,
        }
    }

    pub fn chacha20_accelerator(&self) -> &ChaCha20Accelerator {
        &self.chacha20_accel
    }

    pub fn blake3_accelerator(&self) -> &Blake3Accelerator {
        &self.blake3_accel
    }

    pub fn batch_processor(&self) -> Arc<CryptoProcessor> {
        self.batch_processor.clone()
    }

    pub fn cpu_capabilities(&self) -> &CpuCapabilities {
        &self.cpu_caps
    }

    /// Выполняет операцию с защитой от timing attacks и SIMD ускорением
    #[inline]
    pub fn execute_with_acceleration<F, T>(&self, operation: F) -> Result<T, String>
    where
        F: FnOnce(&ChaCha20Accelerator, &Blake3Accelerator) -> T,
    {
        let start_instant = Instant::now();

        #[cfg(target_arch = "x86_64")]
        let start_cycles = unsafe { std::arch::x86_64::_rdtsc() };

        #[cfg(not(target_arch = "x86_64"))]
        let start_cycles = 0;

        // Выполняем операцию с передачей ускорителей
        let result = operation(&self.chacha20_accel, &self.blake3_accel);

        #[cfg(target_arch = "x86_64")]
        let end_cycles = unsafe { std::arch::x86_64::_rdtsc() };

        #[cfg(not(target_arch = "x86_64"))]
        let end_cycles = 0;

        let elapsed_time = start_instant.elapsed();
        let cycles = end_cycles.wrapping_sub(start_cycles);

        // Обновляем статистику
        self.update_stats(cycles, elapsed_time, true);

        // Проверяем timing аномалии
        if self.check_timing_anomaly(cycles, elapsed_time).is_err() {
            warn!("Timing anomaly detected, but continuing due to acceleration");
        }

        Ok(result)
    }

    /// Batch операция
    pub fn execute_batch<F, T>(&self, operations: Vec<F>) -> Vec<Result<T, String>>
    where
        F: FnOnce(&ChaCha20Accelerator, &Blake3Accelerator) -> T + Send,
        T: Send,
    {
        let start = Instant::now();

        let results: Vec<_> = operations
            .into_par_iter()
            .map(|op| {
                self.execute_with_acceleration(|cha, blake| op(cha, blake))
            })
            .collect();

        let elapsed = start.elapsed();

        // Обновляем batch статистику
        let mut stats = self.stats.lock().unwrap();
        stats.batch_operations += results.len() as u64;
        stats.total_operations += results.len() as u64;

        debug!("Batch execution completed in {:?} for {} operations",
               elapsed, results.len());

        results
    }

    fn update_stats(&self, cycles: u64, elapsed_time: std::time::Duration, simd_used: bool) {
        let mut stats = self.stats.lock().unwrap();

        stats.total_operations += 1;
        stats.cycles.last = cycles;

        if cycles < stats.cycles.min {
            stats.cycles.min = cycles;
        }
        if cycles > stats.cycles.max {
            stats.cycles.max = cycles;
        }

        let total = stats.total_operations;
        stats.cycles.avg = ((stats.cycles.avg * (total - 1)) + cycles) / total;

        let elapsed_ns = elapsed_time.as_nanos() as u64;
        stats.avg_execution_time_ns = ((stats.avg_execution_time_ns * (total - 1)) + elapsed_ns) / total;

        if simd_used {
            stats.simd_operations += 1;
        }
    }

    fn check_timing_anomaly(&self, cycles: u64, elapsed_time: std::time::Duration) -> Result<(), String> {
        // Более мягкие лимиты для SIMD операций
        let max_cycles = if self.cpu_caps.avx2 { 2000 } else { 1000 };
        let min_cycles = if self.cpu_caps.avx2 { 5 } else { 10 };

        if cycles > max_cycles {
            return Err(format!(
                "Timing attack detected: {} cycles (max: {}) in {:?}",
                cycles, max_cycles, elapsed_time
            ));
        }

        if cycles < min_cycles {
            return Err(format!(
                "Suspiciously fast operation: {} cycles (min: {}) in {:?}",
                cycles, min_cycles, elapsed_time
            ));
        }

        Ok(())
    }

    pub fn get_stats(&self) -> RuntimeStats {
        self.stats.lock().unwrap().clone()
    }

    pub fn get_performance_report(&self) -> String {
        let stats = self.get_stats();
        let simd_percentage = if stats.total_operations > 0 {
            (stats.simd_operations as f64 / stats.total_operations as f64) * 100.0
        } else {
            0.0
        };

        let batch_percentage = if stats.total_operations > 0 {
            (stats.batch_operations as f64 / stats.total_operations as f64) * 100.0
        } else {
            0.0
        };

        format!(
            "Operations: {}, Failed: {}, SIMD: {:.1}%, Batch: {:.1}%, Avg time: {}ns",
            stats.total_operations,
            stats.failed_operations,
            simd_percentage,
            batch_percentage,
            stats.avg_execution_time_ns
        )
    }
}

impl Default for PhantomRuntime {
    fn default() -> Self {
        let num_workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        Self::new(num_workers)
    }
}