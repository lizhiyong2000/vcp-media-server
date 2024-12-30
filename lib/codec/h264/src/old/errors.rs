use thiserror::Error;
use vcp_media_common::bytesio::bits_errors::BitError;

#[derive(Debug, Error)]
pub enum H264Error {
    #[error("bit error")]
    BitError(#[from] BitError),
}


