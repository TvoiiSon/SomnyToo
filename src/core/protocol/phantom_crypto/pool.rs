use std::sync::Arc;
use crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto;

pub struct PhantomCryptoPool {
    crypto_instances: Vec<Arc<PhantomCrypto>>,
}

impl PhantomCryptoPool {
    pub fn spawn(num_workers: usize, crypto: Arc<PhantomCrypto>) -> Arc<Self> {
        let mut instances = Vec::with_capacity(num_workers);
        for _ in 0..num_workers {
            instances.push(crypto.clone());
        }

        Arc::new(Self {
            crypto_instances: instances,
        })
    }

    pub fn get_instance(&self, index: usize) -> Option<Arc<PhantomCrypto>> {
        self.crypto_instances.get(index).cloned()
    }
}