use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::HashMap;
use tracing::{debug};

use crate::core::protocol::phantom_crypto::core::keys::PhantomSession;
use crate::core::protocol::error::{ProtocolResult, ProtocolError, CryptoError};
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;

use crate::core::protocol::phantom_crypto::batch::config::BatchConfig;
use crate::core::protocol::phantom_crypto::batch::types::priority::Priority;

/// Криптографическая операция
#[derive(Debug, Clone)]
pub enum CryptoOperation {
    Encrypt {
        session_id: Vec<u8>,
        sequence: u64,
        packet_type: u8,
        plaintext: Vec<u8>,
        key_material: [u8; 32],
    },
    Decrypt {
        session_id: Vec<u8>,
        ciphertext: Vec<u8>,
        expected_sequence: u64,
    },
}

/// Пакет криптографических операций
pub struct CryptoBatch {
    pub id: u64,
    pub operations: Vec<CryptoOperation>,
    pub priority: Priority,
    pub created_at: Instant,
}

impl CryptoBatch {
    pub fn new(id: u64, priority: Priority) -> Self {
        Self {
            id,
            operations: Vec::with_capacity(64),
            priority,
            created_at: Instant::now(),
        }
    }

    pub fn add_operation(&mut self, op: CryptoOperation) {
        self.operations.push(op);
    }

    pub fn len(&self) -> usize {
        self.operations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }
}

/// Результат пакетной обработки
#[derive(Debug)]
pub struct ProcessingResult {
    pub batch_id: u64,
    pub results: Vec<ProtocolResult<Vec<u8>>>,
    pub processing_time: Duration,
    pub successful: usize,
    pub failed: usize,
}

/// Криптопроцессор с поддержкой batch операций
pub struct CryptoProcessor {
    config: BatchConfig,
    packet_processor: PhantomPacketProcessor,
    session_cache: Arc<tokio::sync::RwLock<HashMap<Vec<u8>, Arc<PhantomSession>>>>,

    // Статистика
    total_batches: std::sync::atomic::AtomicU64,
    total_operations: std::sync::atomic::AtomicU64,
    total_failed: std::sync::atomic::AtomicU64,
}

impl CryptoProcessor {
    pub fn new(config: BatchConfig) -> Self {
        Self {
            config,
            packet_processor: PhantomPacketProcessor::new(),
            session_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            total_batches: std::sync::atomic::AtomicU64::new(0),
            total_operations: std::sync::atomic::AtomicU64::new(0),
            total_failed: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Обработка батча шифрования
    pub async fn process_encryption_batch(
        &self,
        batch: CryptoBatch,
        sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>,
    ) -> ProcessingResult {
        let start_time = Instant::now();

        debug!("🔐 Processing encryption batch #{} with {} operations",
               batch.id, batch.len());

        if batch.is_empty() {
            return ProcessingResult {
                batch_id: batch.id,
                results: Vec::new(),
                processing_time: start_time.elapsed(),
                successful: 0,
                failed: 0,
            };
        }

        // Обновляем кэш сессий
        self.update_session_cache(sessions).await;

        // Разделяем операции
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

        let results = if self.config.enable_adaptive_batching && encryption_ops.len() >= 8 {
            // Параллельная обработка для больших батчей
            self.process_encryption_parallel(&encryption_ops, sessions).await
        } else {
            // Последовательная обработка для маленьких батчей
            self.process_encryption_sequential(&encryption_ops, sessions).await
        };

        let processing_time = start_time.elapsed();
        let successful = results.iter().filter(|r| r.is_ok()).count();
        let failed = results.iter().filter(|r| r.is_err()).count();

        // Обновляем статистику
        self.total_batches.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.total_operations.fetch_add(batch.len() as u64, std::sync::atomic::Ordering::Relaxed);
        self.total_failed.fetch_add(failed as u64, std::sync::atomic::Ordering::Relaxed);

        debug!("✅ Encryption batch #{} completed in {:?}: {}/{} successful",
               batch.id, processing_time, successful, batch.len());

        ProcessingResult {
            batch_id: batch.id,
            results,
            processing_time,
            successful,
            failed,
        }
    }

    /// Обработка батча дешифрования
    pub async fn process_decryption_batch(
        &self,
        batch: CryptoBatch,
        sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>,
    ) -> ProcessingResult {
        let start_time = Instant::now();

        debug!("🔓 Processing decryption batch #{} with {} operations",
               batch.id, batch.len());

        // Обновляем кэш сессий
        self.update_session_cache(sessions).await;

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

        let results = if self.config.enable_adaptive_batching && decryption_ops.len() >= 8 {
            // Параллельная обработка для больших батчей
            self.process_decryption_parallel(&decryption_ops, sessions).await
        } else {
            // Последовательная обработка для маленьких батчей
            self.process_decryption_sequential(&decryption_ops, sessions).await
        };

        let processing_time = start_time.elapsed();
        let successful = results.iter().filter(|r| r.is_ok()).count();
        let failed = results.iter().filter(|r| r.is_err()).count();

        // Обновляем статистику
        self.total_batches.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.total_operations.fetch_add(batch.len() as u64, std::sync::atomic::Ordering::Relaxed);
        self.total_failed.fetch_add(failed as u64, std::sync::atomic::Ordering::Relaxed);

        debug!("✅ Decryption batch #{} completed in {:?}: {}/{} successful",
               batch.id, processing_time, successful, batch.len());

        ProcessingResult {
            batch_id: batch.id,
            results,
            processing_time,
            successful,
            failed,
        }
    }

    /// Параллельная обработка шифрования
    async fn process_encryption_parallel(
        &self,
        ops: &[(&Vec<u8>, u64, u8, Vec<u8>, [u8; 32])],
        sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>,
    ) -> Vec<ProtocolResult<Vec<u8>>> {
        // Используем rayon для параллельной обработки
        use rayon::prelude::*;

        ops.par_iter()
            .map(|(session_id, sequence, packet_type, plaintext, key_material)| {
                self.process_single_encryption(
                    sessions.get(*session_id),
                    *sequence,
                    *packet_type,
                    plaintext,
                    *key_material,
                )
            })
            .collect()
    }

    /// Последовательная обработка шифрования
    async fn process_encryption_sequential(
        &self,
        ops: &[(&Vec<u8>, u64, u8, Vec<u8>, [u8; 32])],
        sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>,
    ) -> Vec<ProtocolResult<Vec<u8>>> {
        let mut results = Vec::with_capacity(ops.len());

        for (session_id, sequence, packet_type, plaintext, key_material) in ops {
            results.push(self.process_single_encryption(
                sessions.get(*session_id),
                *sequence,
                *packet_type,
                plaintext,
                *key_material,
            ));
        }

        results
    }

    /// Параллельная обработка дешифрования
    async fn process_decryption_parallel(
        &self,
        ops: &[(&Vec<u8>, Vec<u8>, u64)],
        sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>,
    ) -> Vec<ProtocolResult<Vec<u8>>> {
        // Используем rayon для параллельной обработки
        use rayon::prelude::*;

        ops.par_iter()
            .map(|(session_id, ciphertext, expected_sequence)| {
                self.process_single_decryption(
                    sessions.get(*session_id),
                    ciphertext,
                    *expected_sequence,
                )
            })
            .collect()
    }

    /// Последовательная обработка дешифрования
    async fn process_decryption_sequential(
        &self,
        ops: &[(&Vec<u8>, Vec<u8>, u64)],
        sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>,
    ) -> Vec<ProtocolResult<Vec<u8>>> {
        let mut results = Vec::with_capacity(ops.len());

        for (session_id, ciphertext, expected_sequence) in ops {
            results.push(self.process_single_decryption(
                sessions.get(*session_id),
                ciphertext,
                *expected_sequence,
            ));
        }

        results
    }

    /// Обработка одиночного шифрования
    fn process_single_encryption(
        &self,
        session: Option<&Arc<PhantomSession>>,
        _sequence: u64,  // Добавили префикс _
        packet_type: u8,
        plaintext: &Vec<u8>,
        _key_material: [u8; 32],  // Добавили префикс _
    ) -> ProtocolResult<Vec<u8>> {
        match session {
            Some(session) => {
                // Используем PhantomPacketProcessor для шифрования
                match self.packet_processor.create_outgoing_vec(
                    session,
                    packet_type,
                    plaintext,
                ) {
                    Ok(encrypted_data) => Ok(encrypted_data),
                    Err(e) => Err(ProtocolError::Crypto {
                        source: CryptoError::EncryptionFailed {
                            reason: format!("Encryption failed: {}", e)
                        }
                    }),
                }
            }
            None => Err(ProtocolError::Crypto {
                source: CryptoError::EncryptionFailed {
                    reason: "Session not found".to_string()
                }
            }),
        }
    }

    /// Обработка одиночного дешифрования
    fn process_single_decryption(
        &self,
        session: Option<&Arc<PhantomSession>>,
        ciphertext: &Vec<u8>,
        _expected_sequence: u64,
    ) -> ProtocolResult<Vec<u8>> {
        match session {
            Some(session) => {
                // Используем PhantomPacketProcessor для дешифрования
                match self.packet_processor.process_incoming_vec(ciphertext, session) {
                    Ok((_packet_type, decrypted_data)) => {
                        let mut result = Vec::new();
                        result.push(_packet_type);
                        result.extend_from_slice(&decrypted_data);
                        Ok(result)
                    }
                    Err(e) => Err(ProtocolError::Crypto {
                        source: CryptoError::DecryptionFailed {
                            reason: format!("Decryption failed: {}", e)
                        }
                    }),
                }
            }
            None => Err(ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Session not found".to_string()
                }
            }),
        }
    }

    /// Обновление кэша сессий
    async fn update_session_cache(&self, sessions: &HashMap<Vec<u8>, Arc<PhantomSession>>) {
        let mut cache = self.session_cache.write().await;
        for (session_id, session) in sessions {
            cache.insert(session_id.clone(), session.clone());
        }
    }

    /// Предварительное заполнение кэша сессий
    pub async fn prefill_session_cache(&self, sessions: HashMap<Vec<u8>, Arc<PhantomSession>>) {
        let mut cache = self.session_cache.write().await;
        for (session_id, session) in sessions {
            cache.insert(session_id, session);
        }
    }

    /// Получение статистики
    pub fn get_stats(&self) -> ProcessorStats {
        ProcessorStats {
            total_batches: self.total_batches.load(std::sync::atomic::Ordering::Relaxed),
            total_operations: self.total_operations.load(std::sync::atomic::Ordering::Relaxed),
            total_failed: self.total_failed.load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Очистка кэша
    pub async fn clear_cache(&self) {
        let mut cache = self.session_cache.write().await;
        cache.clear();
    }
}

/// Статистика процессора
#[derive(Debug, Clone)]
pub struct ProcessorStats {
    pub total_batches: u64,
    pub total_operations: u64,
    pub total_failed: u64,
}