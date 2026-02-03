use thiserror::Error;
use std::net::IpAddr;
use tracing::{debug, error, warn};

#[derive(Debug, Error, Clone)]
pub enum ProtocolError {
    #[error("👻 Crypto operation failed: {source}")]
    Crypto {
        #[from]
        source: CryptoError,
    },

    #[error("👻 IO error: {details}")]
    IoError {
        details: String,
    },

    #[error("👻 IO error: {0}")]
    Io(String),

    #[error("👻 Packet format error: {details}")]
    MalformedPacket { details: String },

    #[error("👻 Handshake failed: {reason}")]
    HandshakeFailed { reason: String },

    #[error("👻 Rate limit exceeded for {ip}")]
    RateLimitExceeded { ip: IpAddr },

    #[error("👻 Session error: {details}")]
    SessionError { details: String },

    #[error("👻 Authentication failed: {reason}")]
    AuthenticationFailed { reason: String },

    #[error("👻 Configuration error: {details}")]
    ConfigError { details: String },

    #[error("👻 Timeout occurred after {duration:?}")]
    Timeout { duration: std::time::Duration },

    #[error("👻 Internal server error: {details}")]
    InternalError { details: String },

    #[error("👻 Phantom crypto error: {details}")]
    PhantomCryptoError { details: String },

    #[error("👻 Memory scatter error: {details}")]
    MemoryScatterError { details: String },

    #[error("👻 Hardware acceleration unavailable")]
    HardwareAccelerationUnavailable,

    // ДОБАВЛЯЕМ ЗДЕСЬ ConnectionClosed:
    #[error("👻 Connection closed")]
    ConnectionClosed,
}

#[derive(Debug, Error, Clone)]
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

    #[error("Phantom key assembly failed: {reason}")]
    PhantomKeyAssemblyFailed { reason: String },

    #[error("Memory scattering failed: {reason}")]
    MemoryScatteringFailed { reason: String },
}

// Обновим реализацию автоматического логирования для ProtocolError
impl ProtocolError {
    pub fn log(self) -> Self {
        match &self {
            ProtocolError::RateLimitExceeded { ip } => {
                warn!("👻 Rate limit exceeded for IP: {}", ip);
            }
            ProtocolError::HandshakeFailed { reason } => {
                warn!("👻 Handshake failed: {}", reason);
            }
            ProtocolError::Crypto { source } => {
                error!("👻 Crypto error: {}", source);
            }
            ProtocolError::PhantomCryptoError { details } => {
                error!("👻 Phantom crypto error: {}", details);
            }
            ProtocolError::ConnectionClosed => {
                // ConnectionClosed - это нормальное событие, не ошибка
                // Используем debug вместо error/warn
                debug!("👻 Connection closed");
            }
            _ => {
                error!("👻 Protocol error: {}", self);
            }
        }
        self
    }
}

impl From<hkdf::InvalidLength> for ProtocolError {
    fn from(_err: hkdf::InvalidLength) -> Self {
        ProtocolError::Crypto {
            source: CryptoError::InvalidKeyLength {
                expected: 32, // стандартный размер для HKDF
                actual: 0, // hkdf::InvalidLength не предоставляет фактическую длину
            },
        }
    }
}

// Конвертация из aes_gcm::Error
impl From<aes_gcm::Error> for ProtocolError {
    fn from(err: aes_gcm::Error) -> Self {
        ProtocolError::Crypto {
            source: CryptoError::DecryptionFailed {
                reason: err.to_string(),
            },
        }
    }
}

// Убрана некорректная конвертация для constant_time_eq - эта библиотека не имеет публичного типа ошибок

// Конвертация из std::io::Error
impl From<std::io::Error> for ProtocolError {
    fn from(err: std::io::Error) -> Self {
        // Проверяем, является ли ошибка закрытием соединения
        match err.kind() {
            std::io::ErrorKind::UnexpectedEof |
            std::io::ErrorKind::ConnectionReset |
            std::io::ErrorKind::ConnectionAborted |
            std::io::ErrorKind::BrokenPipe => {
                ProtocolError::ConnectionClosed
            }
            _ => {
                ProtocolError::Io(err.to_string())
            }
        }
    }
}

// Конвертация из anyhow::Error
impl From<anyhow::Error> for ProtocolError {
    fn from(err: anyhow::Error) -> Self {
        ProtocolError::InternalError {
            details: err.to_string(),
        }
    }
}

// Конвертация для hmac::digest::InvalidLength
impl From<digest::InvalidLength> for ProtocolError {
    fn from(_err: digest::InvalidLength) -> Self {
        ProtocolError::Crypto {
            source: CryptoError::InvalidKeyLength {
                expected: 0, // к сожалению, тип не предоставляет эту информацию
                actual: 0,
            },
        }
    }
}

// Конвертация для rand_core::Error
impl From<rand_core::Error> for ProtocolError {
    fn from(err: rand_core::Error) -> Self {
        ProtocolError::PhantomCryptoError {
            details: format!("Random generation failed: {}", err),
        }
    }
}

// Добавляем поддержку std::array::TryFromSliceError
impl From<std::array::TryFromSliceError> for ProtocolError {
    fn from(err: std::array::TryFromSliceError) -> Self {
        ProtocolError::MalformedPacket {
            details: format!("Array conversion failed: {}", err),
        }
    }
}

// Конвертация для tokio::time::error::Elapsed (таймауты)
impl From<tokio::time::error::Elapsed> for ProtocolError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        ProtocolError::Timeout {
            duration: std::time::Duration::from_secs(5), // Default timeout
        }
    }
}

// Добавляем конвертацию для tokio::sync::mpsc::error::SendError
impl<T> From<tokio::sync::mpsc::error::SendError<T>> for ProtocolError {
    fn from(err: tokio::sync::mpsc::error::SendError<T>) -> Self {
        ProtocolError::InternalError {
            details: format!("Channel send error: {}", err),
        }
    }
}

// Конвертация для tokio::sync::oneshot::error::RecvError
impl From<tokio::sync::oneshot::error::RecvError> for ProtocolError {
    fn from(err: tokio::sync::oneshot::error::RecvError) -> Self {
        ProtocolError::InternalError {
            details: format!("Oneshot receive error: {}", err),
        }
    }
}

pub type ProtocolResult<T> = Result<T, ProtocolError>;