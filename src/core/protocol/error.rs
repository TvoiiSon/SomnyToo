use thiserror::Error;
use std::net::IpAddr;
use tracing::{error, warn};

use crate::core::protocol::packets::decoder::packet_parser::DecodeError;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("Crypto operation failed: {source}")]
    Crypto {
        #[from]
        source: CryptoError,
    },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },

    #[error("Packet format error: {details}")]
    MalformedPacket { details: String },

    #[error("Handshake failed: {reason}")]
    HandshakeFailed { reason: String },

    #[error("Rate limit exceeded for {ip}")]
    RateLimitExceeded { ip: IpAddr },

    #[error("Session error: {details}")]
    SessionError { details: String },

    #[error("Authentication failed: {reason}")]
    AuthenticationFailed { reason: String },

    #[error("Configuration error: {details}")]
    ConfigError { details: String },

    #[error("Timeout occurred after {duration:?}")]
    Timeout { duration: std::time::Duration },

    #[error("Internal server error: {details}")]
    InternalError { details: String },

    // УДАЛЯЕМ дублирующий вариант Anyhow и оставляем только одну имплементацию From
}

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("Encryption failed: {reason}")]
    EncryptionFailed { reason: String },

    #[error("Decryption failed: {reason}")]
    DecryptionFailed { reason: String },

    #[error("Key derivation failed")]
    KeyDerivationFailed,

    #[error("Invalid key length: expected {expected}, got {actual}")]
    InvalidKeyLength { expected: usize, actual: usize },

    #[error("HMAC verification failed")]
    HmacVerificationFailed,

    #[error("Nonce reuse detected")]
    NonceReuse,
}

// Реализация автоматического логирования для ProtocolError
impl ProtocolError {
    pub fn log(self) -> Self {
        match &self {
            ProtocolError::RateLimitExceeded { ip } => {
                warn!("Rate limit exceeded for IP: {}", ip);
            }
            ProtocolError::HandshakeFailed { reason } => {
                warn!("Handshake failed: {}", reason);
            }
            ProtocolError::Crypto { source } => {
                error!("Crypto error: {}", source);
            }
            _ => {
                error!("Protocol error: {}", self);
            }
        }
        self
    }
}

// Конвертация из других типов ошибок
impl From<aes_gcm::Error> for ProtocolError {
    fn from(err: aes_gcm::Error) -> Self {
        ProtocolError::Crypto {
            source: CryptoError::EncryptionFailed {
                reason: err.to_string(),
            },
        }
    }
}

impl From<digest::InvalidLength> for ProtocolError {
    fn from(_: digest::InvalidLength) -> Self {
        ProtocolError::Crypto {
            source: CryptoError::InvalidKeyLength {
                expected: 32,
                actual: 0,
            },
        }
    }
}

// ОДНА имплементация для anyhow::Error
impl From<anyhow::Error> for ProtocolError {
    fn from(err: anyhow::Error) -> Self {
        // Пытаемся извлечь io::Error из anyhow::Error
        if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
            ProtocolError::Io {
                source: std::io::Error::new(io_err.kind(), io_err.to_string()),
            }
        } else {
            // Для других anyhow ошибок создаем общий InternalError
            ProtocolError::InternalError {
                details: err.to_string(),
            }
        }
    }
}

// Добавляем конвертацию для DecodeError
impl From<DecodeError> for ProtocolError {
    fn from(err: DecodeError) -> Self {
        match err {
            DecodeError::InvalidLength => ProtocolError::MalformedPacket {
                details: "Invalid packet length".to_string(),
            },
            DecodeError::InvalidMagic => ProtocolError::MalformedPacket {
                details: "Invalid magic bytes".to_string(),
            },
            DecodeError::InvalidSignature => ProtocolError::Crypto {
                source: CryptoError::HmacVerificationFailed,
            },
            DecodeError::DecryptionFailed => ProtocolError::Crypto {
                source: CryptoError::DecryptionFailed {
                    reason: "Packet decryption failed".to_string(),
                },
            },
            DecodeError::InvalidPacketType => ProtocolError::MalformedPacket {
                details: "Invalid packet type".to_string(),
            },
        }
    }
}

pub type ProtocolResult<T> = Result<T, ProtocolError>;