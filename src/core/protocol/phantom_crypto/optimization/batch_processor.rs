use std::sync::Arc;
use std::time::Instant;
use rayon::prelude::*;
use tracing::{info, debug};
use crossbeam::channel::{unbounded, Receiver, Sender};

use crate::core::protocol::phantom_crypto::core::keys::PhantomSession;
use crate::core::protocol::phantom_crypto::packet::{
    PhantomPacketProcessor, MAX_PAYLOAD_SIZE
};
use crate::core::protocol::error::{ProtocolResult, ProtocolError};

/// Batch задач для обработки
pub struct PhantomBatch {
    pub sessions: Vec<Arc<PhantomSession>>,
    pub packet_data: Vec<Vec<u8>>,
    pub outputs: Vec<Vec<u8>>, // Предвыделенные выходные буферы
}

impl PhantomBatch {
    pub fn new(capacity: usize) -> Self {
        Self {
            sessions: Vec::with_capacity(capacity),
            packet_data: Vec::with_capacity(capacity),
            outputs: vec![vec![0u8; MAX_PAYLOAD_SIZE]; capacity],
        }
    }

    pub fn add(&mut self, session: Arc<PhantomSession>, packet_data: Vec<u8>) {
        self.sessions.push(session);
        self.packet_data.push(packet_data);
    }

    pub fn clear(&mut self) {
        self.sessions.clear();
        self.packet_data.clear();
        // Очищаем буферы, но сохраняем память
        for output in &mut self.outputs {
            output.fill(0);
        }
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// Результат batch обработки
pub struct BatchResult {
    pub packet_types: Vec<u8>,
    pub plaintexts: Vec<Vec<u8>>,
    pub errors: Vec<Option<ProtocolError>>,
    pub processing_time: std::time::Duration,
}

/// Высокопроизводительный batch процессор
pub struct PhantomBatchProcessor {
    packet_processor: PhantomPacketProcessor,
    worker_pool: rayon::ThreadPool,
    max_batch_size: usize,
}

impl PhantomBatchProcessor {
    pub fn new(num_threads: usize, max_batch_size: usize) -> Self {
        let worker_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build()
            .unwrap();

        Self {
            packet_processor: PhantomPacketProcessor::new(),
            worker_pool,
            max_batch_size,
        }
    }

    /// Обработка batch параллельно
    pub fn process_batch(&self, mut batch: PhantomBatch) -> BatchResult {
        let start = Instant::now();
        let batch_size = batch.len();

        if batch_size == 0 {
            return BatchResult {
                packet_types: Vec::new(),
                plaintexts: Vec::new(),
                errors: Vec::new(),
                processing_time: start.elapsed(),
            };
        }

        info!("Processing batch of {} packets", batch_size);

        let mut packet_types = vec![0u8; batch_size];
        let mut plaintexts = Vec::with_capacity(batch_size);
        let mut errors = vec![None; batch_size];

        // Создаем векторы для распараллеливания
        let sessions: Vec<_> = batch.sessions.drain(..).collect();
        let packet_data: Vec<_> = batch.packet_data.drain(..).collect();
        let outputs: Vec<_> = batch.outputs.drain(..).collect();

        // Обрабатываем batch в пуле worker-ов
        self.worker_pool.install(|| {
            let results: Vec<_> = (0..batch_size)
                .into_par_iter()
                .map(|i| {
                    let session = &sessions[i];
                    let packet_data = &packet_data[i];
                    let _output = &outputs[i]; // Не мутабельно, но нам нужна копия

                    match self.packet_processor.process_incoming(packet_data, session) {
                        Ok((packet_type, plaintext_slice)) => {
                            // Создаем новый вектор для результата
                            let plaintext = plaintext_slice.to_vec();
                            Ok((packet_type, plaintext))
                        }
                        Err(e) => Err(e),
                    }
                })
                .collect();

            // Собираем результаты
            for (i, result) in results.into_iter().enumerate() {
                match result {
                    Ok((packet_type, plaintext)) => {
                        packet_types[i] = packet_type;
                        plaintexts.push(plaintext);
                        errors[i] = None;
                    }
                    Err(e) => {
                        errors[i] = Some(e);
                        plaintexts.push(Vec::new()); // Placeholder
                    }
                }
            }
        });

        let processing_time = start.elapsed();

        debug!(
            "Batch processing completed in {:?} ({:.2} packets/ms)",
            processing_time,
            batch_size as f64 / processing_time.as_millis() as f64
        );

        BatchResult {
            packet_types,
            plaintexts,
            errors,
            processing_time,
        }
    }

    /// Batch шифрование
    pub fn encrypt_batch(
        &self,
        sessions: &[Arc<PhantomSession>],
        packet_types: &[u8],
        plaintexts: &[Vec<u8>],
    ) -> Vec<ProtocolResult<Vec<u8>>> {
        assert_eq!(sessions.len(), packet_types.len());
        assert_eq!(sessions.len(), plaintexts.len());

        let batch_size = sessions.len();

        self.worker_pool.install(|| {
            (0..batch_size)
                .into_par_iter()
                .map(|i| {
                    self.packet_processor.create_outgoing(
                        &sessions[i],
                        packet_types[i],
                        &plaintexts[i],
                    )
                })
                .collect()
        })
    }

    /// Потоковая batch обработка (для high-throughput)
    pub fn process_stream(
        &self,
        stream: impl Iterator<Item = (Arc<PhantomSession>, Vec<u8>)> + Send + 'static,
    ) -> Receiver<ProtocolResult<(u8, Vec<u8>)>> {
        let (tx, rx): (Sender<_>, Receiver<_>) = unbounded();
        let processor = self.packet_processor.clone();

        // Запускаем worker thread для потоковой обработки
        std::thread::spawn(move || {
            for (session, packet_data) in stream {
                let result = processor.process_incoming(&packet_data, &session);

                if tx.send(result).is_err() {
                    break; // Receiver закрыт
                }
            }
        });

        rx
    }

    pub fn is_empty(&self) -> bool {
        // Возвращаем false, так как процессор всегда готов к работе
        false
    }
}

impl Clone for PhantomBatchProcessor {
    fn clone(&self) -> Self {
        Self {
            packet_processor: PhantomPacketProcessor::new(),
            worker_pool: rayon::ThreadPoolBuilder::new()
                .num_threads(self.worker_pool.current_num_threads())
                .build()
                .unwrap(),
            max_batch_size: self.max_batch_size,
        }
    }
}