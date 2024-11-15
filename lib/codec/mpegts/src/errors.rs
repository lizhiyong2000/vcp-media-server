use {
    vcp_media_common::bytesio::bytes_errors::{BytesReadError, BytesWriteError},
    thiserror::Error,
    std::fmt,
    std::io::Error,
};

#[derive(Debug, Error)]
pub enum MpegTsError {
    #[error("bytes read error")]
    BytesReadError(#[from] BytesReadError),

    #[error("bytes write error")]
    BytesWriteError(#[from] BytesWriteError),

    #[error("io error")]
    IOError(#[from] Error),

    #[error("program number exists")]
    ProgramNumberExists,

    #[error("pmt count execeed")]
    PmtCountExeceed,

    #[error("stream count execeed")]
    StreamCountExeceed,

    #[error("stream not found")]
    StreamNotFound,
}

