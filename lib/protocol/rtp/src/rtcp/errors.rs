use thiserror::Error;
use vcp_media_common::bytesio::bytes_errors::BytesReadError;
use vcp_media_common::bytesio::bytes_errors::BytesWriteError;


#[derive(Debug, Error)]
pub enum RtcpError {
    #[error("bytes read error: {}", _0)]
    BytesReadError(#[from] BytesReadError),
    #[error("bytes write error: {}", _0)]
    BytesWriteError(#[from] BytesWriteError),
}

