use {
    std::{io, time::SystemTimeError},
    thiserror::Error,
    vcp_media_common::bytesio::bytes_errors::{BytesReadError, BytesWriteError},
};

#[derive(Debug, Error)]
pub enum HandshakeError {
    #[error("bytes read error: {}", _0)]
    BytesReadError(#[from] BytesReadError),
    #[error("bytes write error: {}", _0)]
    BytesWriteError(#[from] BytesWriteError),
    #[error("system time error: {}", _0)]
    SysTimeError(#[from] SystemTimeError),
    #[error("digest error: {}", _0)]
    DigestError(#[from] DigestError),
    #[error("Digest not found error")]
    DigestNotFound,
    #[error("s0 version not correct error")]
    S0VersionNotCorrect,
    #[error("io error")]
    IOError(#[from] io::Error),
}




#[derive(Debug, Error)]
pub enum DigestError {
    #[error("bytes read error: {}", _0)]
    BytesReadError(#[from] BytesReadError),
    #[error("digest length not correct")]
    DigestLengthNotCorrect,
    #[error("cannot generate digest")]
    CannotGenerate,
    #[error("unknow schema")]
    UnknowSchema,
}

