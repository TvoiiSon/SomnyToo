use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::HashMap;
use rayon::prelude::*;
use tracing::{info, debug, warn};

pub(crate) use super::config::BatchCryptoConfig;
pub(crate) use super::batch::CryptoBatch;
use super::operation::CryptoOperation;
use super::stats::BatchStats;
use crate::core::protocol::phantom_crypto::core::keys::PhantomSession;
use crate::core::protocol::error::{ProtocolResult, ProtocolError, CryptoError};
use crate::core::protocol::phantom_crypto::packet::{HEADER_MAGIC};

/// Высокопроизводительный пакетный криптопроцессор
pub struct CryptoBatchProcessor {
    config: BatchCryptoConfig,
    session_cache: Arc<tokio::sync::RwLock<HashMap<Vec<u8>, Arc<PhantomSession>>>>,
    operation_counter: std::sync::atomic::AtomicU64,
    batch_counter: std::sync::atomic::AtomicU64,

    // Предвыделенные буферы
    encryption_buffers: std::sync::Mutex<Vec<Vec<u8>>>,
    decryption_buffers: std::sync::Mutex<Vec<Vec<u8>>>,
    current_encryption_buffer: std::sync::atomic::AtomicUsize,
    current_decryption_buffer: std::sync::atomic::AtomicUsize,

    // Статистика
    stats: std::sync::Mutex<BatchStats>,
}

impl CryptoBatchProcessor {
    /// Создает новый процессор с заданной конфигурацией
    pub fn new(config: BatchCryptoConfig) -> Self {
        let config_clone = config.clone();

        // Предвыделяем буферы
        let mut encryption_buffers = Vec::with_capacity(config.max_concurrent_batches * 2);
        let mut decryption_buffers = Vec::with_capacity(config.max_concurrent_batches * 2);

        for _ in 0..config.max_concurrent_batches * 2 {
            encryption_buffers.push(vec![0u8; config.buffer_preallocation_size]);
            decryption_buffers.push(vec![0u8; config.buffer_preallocation_size]);
        }

        info!("🚀 CryptoBatchProcessor initialized:");
        info!("  - Batch size: {}", config.batch_size);
        info!("  - SIMD width: {}", config.simd_width);
        info!("  - Max concurrent batches: {}", config.max_concurrent_batches);
        info!("  - SIMD enabled: {}", config.enable_simd);
        info!("  - Parallel processing: {}", config.enable_parallel);
        info!("  - Session cache size: {}", config.session_cache_size);
        info!("  - Buffer preallocation: {} bytes", config.buffer_preallocation_size);

        Self {
            config: config_clone,
            session_cache: Arc::new(tokio::sync::RwLock::new(HashMap::with_capacity(config.session_cache_size))),
            operation_counter: std::sync::atomic::AtomicU64::new(0),
            batch_counter: std::sync::atomic::AtomicU64::new(0),
            encryption_buffers: std::sync::Mutex::new(encryption_buffers),
            decryption_buffers: std::sync::Mutex::new(decryption_buffers),
            current_encryption_buffer: std::sync::atomic::AtomicUsize::new(0),
            current_decryption_buffer: std::sync::atomic::AtomicUsize::new(0),
            stats: std::sync::Mutex::new(BatchStats::default()),
        }
    }

    /// Обработка батча шифрования (SIMD + параллелизм)
    pub async fn process_encryption_batch(
        &self,
        batch: CryptoBatch,
        sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>,
    ) -> crate::core::protocol::phantom_crypto::batch::types::result::BatchResult {
        let batch_id = self.batch_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let start_time = Instant::now();

        debug!("🔐 Processing encryption batch #{} with {} operations",
               batch_id, batch.len());

        if batch.len() == 0 {
            return crate::core::protocol::phantom_crypto::batch::types::result::BatchResult {
                batch_id,
                results: Vec::new(),
                processing_time: start_time.elapsed(),
                successful: 0,
                failed: 0,
                simd_utilization: 0.0,
            };
        }

        let mut results = Vec::with_capacity(batch.len());
        let mut successful = 0;
        let mut failed = 0;

        // Используем кэш сессий
        let mut session_cache_stats = (0, 0);
        {
            let mut session_cache = self.session_cache.write().await;
            for session in sessions.values() {
                session_cache.insert(session.session_id().to_vec(), session.clone());
                session_cache_stats.0 += 1;
            }
        }

        // Разделяем операции по типам для оптимизации
        let encryption_ops: Vec<_> = batch.operations.iter()
            .filter_map(|op| {
                if let CryptoOperation::Encrypt {
                    session_id,
                    sequence,
                    packet_type,
                    plaintext,
                    key_material
                } = op {
                    Some((session_id, *sequence, *packet_type, plaintext.clone(), *key_material))
                } else {
                    None
                }
            })
            .collect();

        if encryption_ops.is_empty() {
            warn!("Empty encryption batch, skipping");
            return crate::core::protocol::phantom_crypto::batch::types::result::BatchResult {
                batch_id,
                results: vec![Err(ProtocolError::MalformedPacket {
                    details: "Empty batch".to_string()
                }); batch.len()],
                processing_time: start_time.elapsed(),
                successful: 0,
                failed: batch.len(),
                simd_utilization: 0.0,
            };
        }

        // Используем предвыделенные буферы
        let buffer_index = self.current_encryption_buffer.fetch_add(
            1,
            std::sync::atomic::Ordering::Relaxed
        ) % (self.config.max_concurrent_batches * 2);

        let buffer_reused = {
            let buffers = self.encryption_buffers.lock().unwrap();
            buffer_index < buffers.len() && buffers[buffer_index].len() >= self.config.buffer_preallocation_size
        };

        // SIMD-оптимизированное пакетное шифрование
        let simd_results = if self.config.enable_simd && encryption_ops.len() >= 4 {
            self.process_simd_encryption(&encryption_ops, sessions, buffer_index).await
        } else {
            self.process_scalar_encryption(&encryption_ops, sessions, buffer_index).await
        };

        // Собираем результаты
        for result in simd_results {
            match result {
                Ok(data) => {
                    results.push(Ok(data));
                    successful += 1;
                }
                Err(e) => {
                    results.push(Err(e));
                    failed += 1;
                }
            }
        }

        let processing_time = start_time.elapsed();
        let simd_utilization = if self.config.enable_simd && encryption_ops.len() >= 4 {
            (encryption_ops.len() as f64 / batch.len() as f64) * 100.0
        } else {
            0.0
        };

        // Обновляем статистику
        {
            let mut stats = self.stats.lock().unwrap();
            stats.total_batches += 1;
            stats.total_operations += batch.len() as u64;
            if simd_utilization > 0.0 {
                stats.total_simd_operations += encryption_ops.len() as u64;
            }
            stats.total_failed += failed as u64;
            stats.session_cache_hits += session_cache_stats.0;
            stats.session_cache_misses += session_cache_stats.1;
            if buffer_reused {
                stats.buffer_reuse_count += 1;
            } else {
                stats.buffer_allocation_count += 1;
            }
        }

        self.update_stats(batch.len(), processing_time, simd_utilization > 0.0);

        debug!("✅ Encryption batch #{} completed in {:?}: {}/{} successful, SIMD: {:.1}%",
               batch_id, processing_time, successful, batch.len(), simd_utilization);

        crate::core::protocol::phantom_crypto::batch::types::result::BatchResult {
            batch_id,
            results,
            processing_time,
            successful,
            failed,
            simd_utilization,
        }
    }

    /// Обработка батча дешифрования (SIMD + параллелизм)
    pub async fn process_decryption_batch(
        &self,
        batch: CryptoBatch,
        sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>,
    ) -> crate::core::protocol::phantom_crypto::batch::types::result::BatchResult {
        let batch_id = self.batch_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let start_time = Instant::now();

        debug!("🔓 Processing decryption batch #{} with {} operations",
               batch_id, batch.len());

        let mut results = Vec::with_capacity(batch.len());
        let mut successful = 0;
        let mut failed = 0;

        // Используем кэш сессий
        {
            let mut session_cache = self.session_cache.write().await;
            for session in sessions.values() {
                session_cache.insert(session.session_id().to_vec(), session.clone());
            }
        }

        // Разделяем операции
        let decryption_ops: Vec<_> = batch.operations.iter()
            .filter_map(|op| {
                if let CryptoOperation::Decrypt {
                    session_id,
                    ciphertext,
                    expected_sequence
                } = op {
                    Some((session_id, ciphertext.clone(), *expected_sequence))
                } else {
                    None
                }
            })
            .collect();

        // Используем предвыделенные буферы
        let buffer_index = self.current_decryption_buffer.fetch_add(
            1,
            std::sync::atomic::Ordering::Relaxed
        ) % (self.config.max_concurrent_batches * 2);

        // SIMD-оптимизированное пакетное дешифрование
        let simd_results = if self.config.enable_simd && decryption_ops.len() >= 4 {
            self.process_simd_decryption(&decryption_ops, sessions, buffer_index).await
        } else {
            self.process_scalar_decryption(&decryption_ops, sessions, buffer_index).await
        };

        // Собираем результаты
        for result in simd_results.into_iter() {
            match result {
                Ok((packet_type, plaintext)) => {
                    // Для совместимости с существующим кодом
                    let mut full_result = Vec::new();
                    full_result.push(packet_type);
                    full_result.extend_from_slice(&plaintext);
                    results.push(Ok(full_result));
                    successful += 1;
                }
                Err(e) => {
                    results.push(Err(e));
                    failed += 1;
                }
            }
        }

        let processing_time = start_time.elapsed();
        let simd_utilization = if self.config.enable_simd && decryption_ops.len() >= 4 {
            (decryption_ops.len() as f64 / batch.len() as f64) * 100.0
        } else {
            0.0
        };

        // Обновляем статистику
        self.update_stats(batch.len(), processing_time, simd_utilization > 0.0);

        debug!("✅ Decryption batch #{} completed in {:?}: {}/{} successful, SIMD: {:.1}%",
               batch_id, processing_time, successful, batch.len(), simd_utilization);

        crate::core::protocol::phantom_crypto::batch::types::result::BatchResult {
            batch_id,
            results,
            processing_time,
            successful,
            failed,
            simd_utilization,
        }
    }

    /// SIMD-оптимизированное пакетное шифрование
    async fn process_simd_encryption(
        &self,
        ops: &[(&Vec<u8>, u64, u8, Vec<u8>, [u8; 32])],
        sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>,
        buffer_index: usize,
    ) -> Vec<ProtocolResult<Vec<u8>>> {
        let start = Instant::now();
        let batch_size = ops.len();

        // Проверяем доступность буфера
        let buffer_available = {
            let buffers = self.encryption_buffers.lock().unwrap();
            buffer_index < buffers.len()
        };

        if !buffer_available {
            warn!("Buffer index out of range: {}", buffer_index);
            return vec![Err(ProtocolError::MalformedPacket {
                details: "Buffer allocation failed".to_string()
            }); batch_size];
        }

        // Подготавливаем данные для SIMD
        let mut plaintexts = Vec::with_capacity(batch_size);
        let mut keys = Vec::with_capacity(batch_size);
        let mut nonces = Vec::with_capacity(batch_size);
        let mut headers = Vec::with_capacity(batch_size);

        for (session_id, sequence, packet_type, plaintext, key_material) in ops {
            if let Some(session) = sessions.get(*session_id) {
                // Генерируем nonce
                let mut nonce = [0u8; 12];
                let _ = getrandom::getrandom(&mut nonce);

                // Формируем заголовок
                let mut header = vec![0u8; 37]; // Размер заголовка
                self.prepare_header(session, *sequence, *packet_type, &mut header);

                plaintexts.push(plaintext.clone());
                keys.push(*key_material);
                nonces.push(nonce);
                headers.push(header);
            }
        }

        // Пакетное шифрование ChaCha20 (используем скалярную реализацию пока)
        let encrypted_data = self.process_scalar_encryption(ops, sessions, buffer_index).await;

        // Формируем финальные пакеты
        let mut results = Vec::with_capacity(batch_size);
        for i in 0..batch_size {
            if i < encrypted_data.len() && i < nonces.len() && i < headers.len() {
                let mut final_packet = headers[i].clone();
                final_packet.extend_from_slice(&nonces[i]);

                match &encrypted_data[i] {
                    Ok(data) => {
                        final_packet.extend_from_slice(data);
                        // Добавляем TAG (заглушка)
                        final_packet.extend_from_slice(&[0u8; 16]);
                        // Добавляем подпись (заглушка)
                        final_packet.extend_from_slice(&[0u8; 32]);
                        results.push(Ok(final_packet));
                    }
                    Err(e) => {
                        results.push(Err(e.clone()));
                    }
                }
            } else {
                results.push(Err(ProtocolError::MalformedPacket {
                    details: "Batch processing error".to_string()
                }));
            }
        }

        let elapsed = start.elapsed();
        debug!("SIMD encryption batch: {} ops in {:?} ({:.1} ops/ms)",
               batch_size, elapsed, batch_size as f64 / elapsed.as_millis() as f64);

        results
    }

    /// SIMD-оптимизированное пакетное дешифрование
    async fn process_simd_decryption(
        &self,
        ops: &[(&Vec<u8>, Vec<u8>, u64)],
        sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>,
        buffer_index: usize,
    ) -> Vec<ProtocolResult<(u8, Vec<u8>)>> {
        let start = Instant::now();
        let batch_size = ops.len();

        // Проверяем доступность буфера
        let buffer_available = {
            let buffers = self.decryption_buffers.lock().unwrap();
            buffer_index < buffers.len()
        };

        if !buffer_available {
            warn!("Buffer index out of range: {}", buffer_index);
            return vec![Err(ProtocolError::MalformedPacket {
                details: "Buffer allocation failed".to_string()
            }); batch_size];
        }

        // Парсим пакеты
        let mut ciphertexts = Vec::with_capacity(batch_size);
        let mut keys = Vec::with_capacity(batch_size);
        let mut nonces = Vec::with_capacity(batch_size);
        let mut expected_sequences = Vec::with_capacity(batch_size);

        for (session_id, ciphertext, expected_sequence) in ops {
            if let Some(_session) = sessions.get(*session_id) {
                // Парсим пакет для извлечения nonce и данных
                if let Ok((nonce, encrypted_data)) = self.parse_packet_batch(ciphertext) {
                    // Генерируем ключ для этой последовательности
                    // Временная заглушка - используем нулевой ключ
                    let key = [0u8; 32];

                    ciphertexts.push(encrypted_data);
                    keys.push(key);
                    nonces.push(nonce);
                    expected_sequences.push(*expected_sequence);
                }
            }
        }

        if ciphertexts.is_empty() {
            return vec![Err(ProtocolError::MalformedPacket {
                details: "No valid packets in batch".to_string()
            }); batch_size];
        }

        // Пакетное дешифрование (скалярная реализация)
        let decrypted_data = self.process_scalar_decryption(ops, sessions, buffer_index).await;

        // Формируем результаты
        let mut results = Vec::with_capacity(batch_size);
        for i in 0..batch_size {
            if i < decrypted_data.len() {
                match &decrypted_data[i] {
                    Ok((packet_type, plaintext)) => {
                        results.push(Ok((*packet_type, plaintext.clone())));
                    }
                    Err(e) => {
                        results.push(Err(e.clone()));
                    }
                }
            } else {
                results.push(Err(ProtocolError::AuthenticationFailed {
                    reason: "Batch verification failed".to_string()
                }));
            }
        }

        let elapsed = start.elapsed();
        debug!("SIMD decryption batch: {} ops in {:?} ({:.1} ops/ms)",
               batch_size, elapsed, batch_size as f64 / elapsed.as_millis() as f64);

        results
    }

    /// Скалярная обработка (fallback)
    async fn process_scalar_encryption(
        &self,
        ops: &[(&Vec<u8>, u64, u8, Vec<u8>, [u8; 32])],
        sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>,
        buffer_index: usize,
    ) -> Vec<ProtocolResult<Vec<u8>>> {
        let batch_size = ops.len();

        if self.config.enable_parallel && batch_size >= self.config.min_batch_for_parallel {
            // Параллельная скалярная обработка
            ops.par_iter()
                .map(|(session_id, sequence, packet_type, plaintext, key_material)| {
                    self.process_single_encryption(
                        sessions.get(*session_id),
                        *sequence,
                        *packet_type,
                        plaintext,
                        *key_material,
                        buffer_index,
                    )
                })
                .collect()
        } else {
            // Последовательная обработка
            let mut results = Vec::with_capacity(batch_size);
            for (session_id, sequence, packet_type, plaintext, key_material) in ops {
                results.push(self.process_single_encryption(
                    sessions.get(*session_id),
                    *sequence,
                    *packet_type,
                    plaintext,
                    *key_material,
                    buffer_index,
                ));
            }
            results
        }
    }

    /// Скалярная обработка дешифрования (fallback)
    async fn process_scalar_decryption(
        &self,
        ops: &[(&Vec<u8>, Vec<u8>, u64)],
        sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>,
        buffer_index: usize,
    ) -> Vec<ProtocolResult<(u8, Vec<u8>)>> {
        let batch_size = ops.len();

        if self.config.enable_parallel && batch_size >= self.config.min_batch_for_parallel {
            ops.par_iter()
                .map(|(session_id, ciphertext, expected_sequence)| {
                    self.process_single_decryption(
                        sessions.get(*session_id),
                        ciphertext,
                        *expected_sequence,
                        buffer_index,
                    )
                })
                .collect()
        } else {
            let mut results = Vec::with_capacity(batch_size);
            for (session_id, ciphertext, expected_sequence) in ops {
                results.push(self.process_single_decryption(
                    sessions.get(*session_id),
                    ciphertext,
                    *expected_sequence,
                    buffer_index,
                ));
            }
            results
        }
    }

    /// Обработка одиночного шифрования
    pub fn process_single_encryption(
        &self,
        _session: Option<&Arc<PhantomSession>>,
        _sequence: u64,
        _packet_type: u8,
        plaintext: &Vec<u8>,
        _key_material: [u8; 32],
        buffer_index: usize,
    ) -> ProtocolResult<Vec<u8>> {
        // Используем буфер если доступен
        {
            let mut buffers = self.encryption_buffers.lock().unwrap();
            if buffer_index < buffers.len() {
                let buffer = &mut buffers[buffer_index];
                if buffer.len() >= plaintext.len() {
                    buffer[..plaintext.len()].copy_from_slice(plaintext);
                    let mut result = buffer[..plaintext.len()].to_vec();
                    result.truncate(plaintext.len());
                    return Ok(result);
                }
            }
        }

        // TODO: Интеграция с существующим PhantomPacketProcessor
        // Временная реализация - возвращаем данные как есть
        Ok(plaintext.clone())
    }

    /// Обработка одиночного дешифрования
    pub fn process_single_decryption(
        &self,
        _session: Option<&Arc<PhantomSession>>,
        ciphertext: &Vec<u8>,
        _expected_sequence: u64,
        buffer_index: usize,
    ) -> ProtocolResult<(u8, Vec<u8>)> {
        // Используем буфер если доступен
        {
            let mut buffers = self.decryption_buffers.lock().unwrap();
            if buffer_index < buffers.len() {
                let buffer = &mut buffers[buffer_index];
                if buffer.len() >= ciphertext.len() {
                    buffer[..ciphertext.len()].copy_from_slice(ciphertext);
                    buffer.truncate(ciphertext.len());
                }
            }
        }

        // TODO: Интеграция с существующим PhantomPacketProcessor
        // Временная реализация - возвращаем данные как есть
        if ciphertext.is_empty() {
            return Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Empty ciphertext".to_string()
                }
            });
        }

        // Предполагаем, что первый байт - тип пакета
        let packet_type = if !ciphertext.is_empty() { ciphertext[0] } else { 0 };
        let data = ciphertext[1..].to_vec();

        Ok((packet_type, data))
    }

    /// Подготовка заголовка пакета
    fn prepare_header(&self, _session: &PhantomSession, _sequence: u64, _packet_type: u8, buffer: &mut [u8]) {
        if buffer.len() >= 2 {
            buffer[0..2].copy_from_slice(&HEADER_MAGIC);
        }
        // TODO: Заполнить остальные поля
    }

    /// Парсинг пакета для batch обработки
    fn parse_packet_batch(&self, data: &[u8]) -> Result<([u8; 12], Vec<u8>), ProtocolError> {
        // Простая реализация парсинга
        if data.len() < 12 {
            return Err(ProtocolError::MalformedPacket {
                details: "Packet too short".to_string()
            });
        }

        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&data[0..12]);

        let encrypted_data = data[12..].to_vec();

        Ok((nonce, encrypted_data))
    }

    /// Обновление статистики
    fn update_stats(&self, batch_size: usize, processing_time: Duration, simd_used: bool) {
        let mut stats = self.stats.lock().unwrap();

        stats.total_batches += 1;
        stats.total_operations += batch_size as u64;

        if simd_used {
            stats.total_simd_operations += batch_size as u64;
        }

        // Скользящее среднее
        let total_batches = stats.total_batches as f64;
        stats.avg_batch_size =
            (stats.avg_batch_size * (total_batches - 1.0) + batch_size as f64) / total_batches;

        stats.avg_processing_time_ns =
            (stats.avg_processing_time_ns * (total_batches - 1.0) + processing_time.as_nanos() as f64) / total_batches;

        // Расчет throughput
        if processing_time.as_nanos() > 0 {
            let ops_per_ns = batch_size as f64 / processing_time.as_nanos() as f64;
            stats.throughput_ops_per_sec = ops_per_ns * 1_000_000_000.0;
        }
    }

    /// Получение статистики
    pub fn get_stats(&self) -> BatchStats {
        self.stats.lock().unwrap().clone()
    }

    /// Сброс статистики
    pub fn reset_stats(&self) {
        *self.stats.lock().unwrap() = BatchStats::default();
    }

    /// Оптимизация размера батча на основе статистики
    pub fn optimize_batch_size(&mut self) -> usize {
        let stats = self.get_stats();

        // Простая эвристика:
        // Если среднее время обработки < 1ms, увеличиваем batch size
        // Если > 5ms, уменьшаем
        if stats.avg_processing_time_ns < 1_000_000.0 && self.config.batch_size < 512 {
            self.config.batch_size = (self.config.batch_size * 2).min(512);
        } else if stats.avg_processing_time_ns > 5_000_000.0 && self.config.batch_size > 32 {
            self.config.batch_size = (self.config.batch_size / 2).max(32);
        }

        self.config.batch_size
    }

    /// Получение кэша сессий
    pub async fn get_session_cache(&self) -> Arc<tokio::sync::RwLock<HashMap<Vec<u8>, Arc<PhantomSession>>>> {
        self.session_cache.clone()
    }

    /// Получение счетчика операций
    pub fn get_operation_counter(&self) -> u64 {
        self.operation_counter.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Получение буферов шифрования
    pub fn get_encryption_buffers(&self) -> Vec<Vec<u8>> {
        self.encryption_buffers.lock().unwrap().clone()
    }

    /// Получение буферов дешифрования
    pub fn get_decryption_buffers(&self) -> Vec<Vec<u8>> {
        self.decryption_buffers.lock().unwrap().clone()
    }

    /// Очистка буферов
    pub fn clear_buffers(&mut self) {
        let mut encryption_buffers = self.encryption_buffers.lock().unwrap();
        let mut decryption_buffers = self.decryption_buffers.lock().unwrap();

        encryption_buffers.clear();
        decryption_buffers.clear();

        // Предвыделяем новые буферы
        for _ in 0..self.config.max_concurrent_batches * 2 {
            encryption_buffers.push(vec![0u8; self.config.buffer_preallocation_size]);
            decryption_buffers.push(vec![0u8; self.config.buffer_preallocation_size]);
        }

        self.current_encryption_buffer.store(0, std::sync::atomic::Ordering::Relaxed);
        self.current_decryption_buffer.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Предварительное заполнение кэша сессий
    pub async fn prefill_session_cache(&self, sessions: HashMap<Vec<u8>, Arc<PhantomSession>>) {
        let mut cache = self.session_cache.write().await;
        for (session_id, session) in sessions {
            cache.insert(session_id, session);
        }
        info!("Prefilled session cache with {} sessions", cache.len());
    }
}