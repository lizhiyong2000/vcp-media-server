use {
    vcp_media_common::bytesio::bytes_errors::{BytesReadError, BytesWriteError},
    thiserror::Error,
    std::{
        fmt, {io, string},
    },
};

#[derive(Debug, Error)]
pub enum Amf0ReadError {
    #[error("Encountered unknown marker: {}", marker)]
    UnknownMarker { marker: u8 },
    #[error("parser string error: {}", _0)]
    StringParseError(#[from] string::FromUtf8Error),
    #[error("bytes read error :{}", _0)]
    BytesReadError(#[from] BytesReadError),
    #[error("wrong type")]
    WrongType,
}




#[derive(Debug, Error)]
pub enum Amf0WriteError {
    #[error("normal string too long")]
    NormalStringTooLong,
    #[error("io error")]
    BufferWriteError(#[from] io::Error),
    #[error("bytes write error")]
    BytesWriteError(#[from] BytesWriteError),
}




