use vcp_media_common::bytesio::bits_errors::BitError;
use thiserror::Error;
use std::fmt;

#[derive(Debug, Error)]
pub enum H264Error {
    #[error("bit error")]
    BitError(#[from] BitError),
}


