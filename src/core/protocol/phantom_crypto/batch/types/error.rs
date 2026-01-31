#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    #[error("Backpressure: too many pending operations")]
    Backpressure,
    #[error("Channel error: {0}")]
    ChannelError(String),
    #[error("Processing timeout")]
    Timeout,
    #[error("Batch processing failed: {0}")]
    ProcessingFailed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum BatchReaderError {
    #[error("Connection already registered")]
    ConnectionAlreadyRegistered,
    #[error("Connection closed")]
    ConnectionClosed,
    #[error("Read timeout")]
    ReadTimeout,
    #[error("Frame read error: {0}")]
    FrameReadError(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum BatchWriterError {
    #[error("Connection already registered")]
    ConnectionAlreadyRegistered,
    #[error("Connection not found")]
    ConnectionNotFound,
    #[error("Write error: {0}")]
    WriteError(String),
    #[error("Queue error: {0}")]
    QueueError(String),
    #[error("Backpressure: too many pending writes")]
    Backpressure,
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}