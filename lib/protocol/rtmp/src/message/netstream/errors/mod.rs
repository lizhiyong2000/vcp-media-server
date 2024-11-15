use crate::message::chunk::errors::PackError;
use vcp_media_flv::amf0::errors::Amf0WriteError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetStreamError {
    #[error("amf0 write error: {}", _0)]
    Amf0WriteError(#[from] Amf0WriteError),
    #[error("invalid max chunk size")]
    InvalidMaxChunkSize { chunk_size: usize },
    #[error("pack error")]
    PackError(#[from] PackError),
}
