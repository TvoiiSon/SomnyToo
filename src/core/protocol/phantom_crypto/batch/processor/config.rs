/// Конфигурация пакетной обработки
#[derive(Debug, Clone)]
pub struct BatchCryptoConfig {
    pub batch_size: usize,           // Размер батча (128, 256, 512)
    pub simd_width: usize,           // Ширина SIMD (4, 8, 16)
    pub max_concurrent_batches: usize,
    pub enable_simd: bool,
    pub enable_parallel: bool,
    pub min_batch_for_parallel: usize,
    pub session_cache_size: usize,
    pub buffer_preallocation_size: usize,
}

impl Default for BatchCryptoConfig {
    fn default() -> Self {
        Self {
            batch_size: 128,
            simd_width: 8,
            max_concurrent_batches: 16,
            enable_simd: true,
            enable_parallel: true,
            min_batch_for_parallel: 32,
            session_cache_size: 1000,
            buffer_preallocation_size: 65536 * 2, // 128KB
        }
    }
}