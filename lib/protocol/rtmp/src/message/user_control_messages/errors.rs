use {
    vcp_media_common::bytesio::bytes_errors::{BytesReadError, BytesWriteError},
    thiserror::Error,
    std::fmt,
    vcp_media_flv::amf0::errors::Amf0WriteError,
};



#[derive(Debug, Error)]
pub enum EventMessagesError {
    #[error("amf0 write error: {}", _0)]
    Amf0WriteError(#[from] Amf0WriteError),
    #[error("bytes write error: {}", _0)]
    BytesWriteError(#[from] BytesWriteError),
    #[error("bytes read error: {}", _0)]
    BytesReadError(#[from] BytesReadError),
    #[error("unknow event session type")]
    UnknowEventMessageType,
}
