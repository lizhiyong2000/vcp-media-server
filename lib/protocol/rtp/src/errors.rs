use {
    thiserror::Error,
    std::fmt,
};

use vcp_media_common::bytesio::bytes_errors::BytesReadError;
use vcp_media_common::bytesio::bytes_errors::BytesWriteError;


#[derive(Debug, Error)]
pub enum PackerError {
    #[error("bytes read error: {}", _0)]
    BytesReadError(#[from] BytesReadError),
    #[error("bytes write error: {}", _0)]
    BytesWriteError(#[from] BytesWriteError),
}


#[derive(Debug, Error)]
pub enum UnPackerError {
    #[error("bytes read error: {}", _0)]
    BytesReadError(#[from] BytesReadError),
    #[error("bytes write error: {}", _0)]
    BytesWriteError(#[from] BytesWriteError),
}


