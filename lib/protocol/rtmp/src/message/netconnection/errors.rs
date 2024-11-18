use {
    crate::message::chunk::errors::PackError,
    thiserror::Error
    ,
    vcp_media_flv::amf0::errors::{Amf0ReadError, Amf0WriteError},
};


#[derive(Debug, Error)]
pub enum NetConnectionError {
    #[error("amf0 write error: {}", _0)]
    Amf0WriteError(#[from] Amf0WriteError),
    #[error("amf0 read error: {}", _0)]
    Amf0ReadError(#[from] Amf0ReadError),
    #[error("pack error")]
    PackError(#[from] PackError),
}
