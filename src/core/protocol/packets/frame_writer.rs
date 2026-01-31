use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::time::timeout;
use tracing::debug;

use crate::core::protocol::error::{ProtocolResult, ProtocolError};

pub async fn write_frame<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    data: &[u8],
) -> ProtocolResult<()> {
    let header = (data.len() as u32).to_be_bytes();

    match timeout(Duration::from_secs(5), writer.write_all(&header)).await {
        Ok(result) => match result {
            Ok(_) => {},
            Err(e) => return Err(ProtocolError::MalformedPacket {
                details: format!("IO error: {}", e)
            }),
        },
        Err(_) => return Err(ProtocolError::Timeout {
            duration: Duration::from_secs(5)
        }),
    }

    match timeout(Duration::from_secs(5), writer.write_all(data)).await {
        Ok(result) => match result {
            Ok(_) => {},
            Err(e) => return Err(ProtocolError::MalformedPacket {
                details: format!("IO error: {}", e)
            }),
        },
        Err(_) => return Err(ProtocolError::Timeout {
            duration: Duration::from_secs(5)
        }),
    }

    match timeout(Duration::from_secs(5), writer.flush()).await {
        Ok(result) => match result {
            Ok(_) => {},
            Err(e) => return Err(ProtocolError::MalformedPacket {
                details: format!("IO error: {}", e)
            }),
        },
        Err(_) => return Err(ProtocolError::Timeout {
            duration: Duration::from_secs(5)
        }),
    }

    debug!("Wrote frame of {} bytes", data.len());
    Ok(())
}