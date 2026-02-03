use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::time::timeout;
use tracing::debug;

use crate::core::protocol::error::{ProtocolResult, ProtocolError};

const MAX_FRAME_SIZE: usize = 65536;
const HEADER_SIZE: usize = 4;

pub async fn read_frame<R: AsyncReadExt + Unpin + ?Sized>(  // <-- Добавили ?Sized
                                                            reader: &mut R,
) -> ProtocolResult<Vec<u8>> {
    let mut header = [0u8; HEADER_SIZE];

    match timeout(Duration::from_secs(30), reader.read_exact(&mut header)).await {
        Ok(result) => match result {
            Ok(_) => {},
            Err(e) => {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    return Ok(Vec::new());
                }
                return Err(ProtocolError::MalformedPacket {
                    details: format!("IO error: {}", e)
                });
            }
        },
        Err(_) => {
            return Err(ProtocolError::Timeout {
                duration: Duration::from_secs(10)
            });
        }
    }

    let length = u32::from_be_bytes(header) as usize;

    if length > MAX_FRAME_SIZE {
        return Err(ProtocolError::MalformedPacket {
            details: format!("Frame too large: {} > {}", length, MAX_FRAME_SIZE)
        });
    }

    if length == 0 {
        return Ok(Vec::new());
    }

    let mut data = vec![0u8; length];

    match timeout(Duration::from_secs(30), reader.read_exact(&mut data)).await {
        Ok(result) => match result {
            Ok(_) => {},
            Err(e) => {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    return Ok(Vec::new());
                }
                return Err(ProtocolError::MalformedPacket {
                    details: format!("IO error: {}", e)
                });
            }
        },
        Err(_) => {
            return Err(ProtocolError::Timeout {
                duration: Duration::from_secs(30)
            });
        }
    }

    debug!("Read frame of {} bytes", data.len());
    Ok(data)
}