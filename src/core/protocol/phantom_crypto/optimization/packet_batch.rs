use std::sync::Arc;
use std::time::Instant;
use rayon::iter::IntoParallelIterator;
use rayon::prelude::*;

use crate::core::protocol::phantom_crypto::{
    core::keys::PhantomSession,
    packet::{PhantomPacketProcessor, MAX_PAYLOAD_SIZE}
};
use crate::core::protocol::error::ProtocolResult;

/// Пакетный обработчик с предвыделенной памятью
pub struct PhantomPacketBatch {
    packets: Vec<Vec<u8>>,
    sessions: Vec<Arc<PhantomSession>>,
    results: Vec<ProtocolResult<(u8, Vec<u8>)>>,
    buffers: Vec<Vec<u8>>, // Предвыделенные буферы
}

impl PhantomPacketBatch {
    pub fn new(capacity: usize) -> Self {
        let mut buffers = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            buffers.push(vec![0u8; MAX_PAYLOAD_SIZE * 2]); // Увеличиваем буфер для разделения
        }

        Self {
            packets: Vec::with_capacity(capacity),
            sessions: Vec::with_capacity(capacity),
            results: Vec::with_capacity(capacity),
            buffers,
        }
    }

    pub fn add(&mut self, session: Arc<PhantomSession>, packet: Vec<u8>) {
        self.packets.push(packet);
        self.sessions.push(session);
        self.results.push(Ok((0, Vec::new()))); // Заполнитель
    }

    pub fn clear(&mut self) {
        self.packets.clear();
        self.sessions.clear();
        self.results.clear();
        // Не очищаем буферы, они переиспользуются
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    pub fn take_results(&mut self) -> Vec<ProtocolResult<(u8, Vec<u8>)>> {
        std::mem::take(&mut self.results)
    }
}

/// Оптимизированный batch процессор
pub struct PhantomBatchProcessor {
    packet_processor: PhantomPacketProcessor,
}

impl PhantomBatchProcessor {
    pub fn new() -> Self {
        Self {
            packet_processor: PhantomPacketProcessor::new(),
        }
    }

    /// Пакетная обработка без аллокаций
    pub fn process_batch_noalloc(
        &self,
        batch: &mut PhantomPacketBatch,
    ) {
        let start = Instant::now();

        for i in 0..batch.len() {
            if let Some(buffer) = batch.buffers.get_mut(i) {
                // Разделяем буфер на work и output части
                let split_point = MAX_PAYLOAD_SIZE + 48;
                if buffer.len() >= split_point + MAX_PAYLOAD_SIZE {
                    let (work_buffer, rest) = buffer.split_at_mut(split_point);
                    let output_buffer = &mut rest[..MAX_PAYLOAD_SIZE];

                    match self.packet_processor.process_incoming_slice(
                        &batch.packets[i],
                        &batch.sessions[i],
                        &mut work_buffer[..MAX_PAYLOAD_SIZE + 48],
                        output_buffer,
                    ) {
                        Ok((packet_type, size)) => {
                            let data = output_buffer[..size].to_vec();
                            batch.results[i] = Ok((packet_type, data));
                        }
                        Err(e) => {
                            batch.results[i] = Err(e);
                        }
                    }
                }
            }
        }

        let elapsed = start.elapsed();
        tracing::debug!("Batch processing completed in {:?} for {} packets", elapsed, batch.len());
    }

    /// Параллельная пакетная обработка с rayon
    pub fn process_batch_parallel(
        &self,
        batch: &mut PhantomPacketBatch,
    ) {
        let start = Instant::now();

        let packet_processor = &self.packet_processor;
        let batch_size = batch.len();

        let results: Vec<_> = (0..batch_size)
            .into_par_iter()
            .map(|i| {
                let mut work_buffer = vec![0u8; MAX_PAYLOAD_SIZE + 48];
                let mut output_buffer = vec![0u8; MAX_PAYLOAD_SIZE];

                match packet_processor.process_incoming_slice(
                    &batch.packets[i],
                    &batch.sessions[i],
                    &mut work_buffer,
                    &mut output_buffer,
                ) {
                    Ok((packet_type, size)) => {
                        let data = output_buffer[..size].to_vec();
                        Ok((packet_type, data))
                    }
                    Err(e) => Err(e),
                }
            })
            .collect();

        batch.results = results;

        let elapsed = start.elapsed();
        tracing::debug!("Parallel batch processing completed in {:?} for {} packets", elapsed, batch_size);
    }

    /// Шифрование batch - теперь с векторами вместо срезов
    pub fn encrypt_batch_parallel(
        &self,
        sessions: &[Arc<PhantomSession>],
        packet_types: &[u8],
        plaintexts: &[Vec<u8>],
    ) -> Vec<ProtocolResult<Vec<u8>>> {
        assert_eq!(sessions.len(), packet_types.len());
        assert_eq!(sessions.len(), plaintexts.len());

        let packet_processor = &self.packet_processor;

        (0..sessions.len())
            .into_par_iter()
            .map(|i| {
                packet_processor.create_outgoing(
                    &sessions[i],
                    packet_types[i],
                    &plaintexts[i],
                )
            })
            .collect()
    }
}

impl Clone for PhantomBatchProcessor {
    fn clone(&self) -> Self {
        Self {
            packet_processor: self.packet_processor.clone(),
        }
    }
}

impl Default for PhantomBatchProcessor {
    fn default() -> Self {
        Self::new()
    }
}