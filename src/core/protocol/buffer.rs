use bytes::{Bytes};
use std::ops::{Deref};

/// Упрощенная zero-copy обертка для обратной совместимости
#[derive(Debug, Clone)]
pub struct PacketBuffer {
    data: Bytes,
}

impl PacketBuffer {
    pub fn new(data: Bytes) -> Self {
        Self { data }
    }

    pub fn from_vec(data: Vec<u8>) -> Self {
        Self::new(Bytes::from(data))
    }

    pub fn into_bytes(self) -> Bytes {
        self.data
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.data.to_vec()
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl Deref for PacketBuffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl AsRef<[u8]> for PacketBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

impl From<Vec<u8>> for PacketBuffer {
    fn from(vec: Vec<u8>) -> Self {
        Self::from_vec(vec)
    }
}

impl From<Bytes> for PacketBuffer {
    fn from(bytes: Bytes) -> Self {
        Self::new(bytes)
    }
}