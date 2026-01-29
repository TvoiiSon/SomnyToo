use std::sync::Arc;
use std::time::{Instant, Duration};
use tokio::sync::Semaphore;
use tracing::{warn, info};

use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;
use crate::core::protocol::error::ProtocolResult;
use crate::core::protocol::phantom_crypto::{
    core::keys::PhantomSession,
    runtime::runtime::PhantomRuntime,
    optimization::batch_processor::PhantomBatchProcessor,
};

/// Упрощенный криптографический пул
pub struct PhantomCryptoPool {
    runtime: Arc<PhantomRuntime>,
    batch_processor: Arc<PhantomBatchProcessor>,
    packet_processor: Arc<PhantomPacketProcessor>,
    concurrency_limiter: Arc<Semaphore>,
}

impl PhantomCryptoPool {
    pub fn spawn(num_workers: usize) -> Self {
        let runtime = Arc::new(PhantomRuntime::new(num_workers));
        let batch_processor = Arc::new(PhantomBatchProcessor::new(num_workers, 100));
        let packet_processor = Arc::new(PhantomPacketProcessor::new());

        let concurrency_limiter = Arc::new(Semaphore::new(num_workers * 10));

        info!("✅ PhantomCryptoPool initialized with {} workers", num_workers);
        info!("  - Concurrency limit: {}", num_workers * 10);
        info!("  - Batch processor ready: true");
        info!("  - Packet processor: ready");

        Self {
            runtime,
            batch_processor,
            packet_processor,
            concurrency_limiter,
        }
    }

    #[inline]
    pub async fn decrypt(
        &self,
        session: Arc<PhantomSession>,
        payload: Vec<u8>,
    ) -> ProtocolResult<(u8, Vec<u8>)> {
        let start = Instant::now();

        let _permit = self.concurrency_limiter.acquire().await.unwrap();

        let result = self.packet_processor.process_incoming_vec(&payload, &session);

        let elapsed = start.elapsed();
        if elapsed > Duration::from_millis(5) {
            warn!("Slow decryption: {:?}", elapsed);
        }

        result
    }

    #[inline]
    pub async fn encrypt(
        &self,
        session: Arc<PhantomSession>,
        packet_type: u8,
        plaintext: Vec<u8>,
    ) -> ProtocolResult<Vec<u8>> {
        let start = Instant::now();

        let _permit = self.concurrency_limiter.acquire().await.unwrap();

        let result = self.packet_processor.create_outgoing_vec(&session, packet_type, &plaintext);

        let elapsed = start.elapsed();
        if elapsed > Duration::from_millis(3) {
            warn!("Slow encryption: {:?}", elapsed);
        }

        result
    }

    pub fn runtime(&self) -> &Arc<PhantomRuntime> {
        &self.runtime
    }

    pub fn get_batch_processor(&self) -> &Arc<PhantomBatchProcessor> {
        &self.batch_processor
    }

    pub fn get_packet_processor(&self) -> &Arc<PhantomPacketProcessor> {
        &self.packet_processor
    }

    pub fn get_stats(&self) -> String {
        format!(
            "PhantomCryptoPool: concurrency={}",
            self.concurrency_limiter.available_permits()
        )
    }
}