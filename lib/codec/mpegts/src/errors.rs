use {
    vcp_media_common::bytesio::bytes_errors::{BytesReadError, BytesWriteError},
    thiserror::Error,
    std::fmt,
    std::io::Error,
};

#[derive(Debug, Error)]
pub enum MpegTsError {
    #[error("bytes read error")]
    BytesReadError(BytesReadError),

    #[error("bytes write error")]
    BytesWriteError(BytesWriteError),

    #[error("io error")]
    IOError(Error),

    #[error("program number exists")]
    ProgramNumberExists,

    #[error("pmt count execeed")]
    PmtCountExeceed,

    #[error("stream count execeed")]
    StreamCountExeceed,

    #[error("stream not found")]
    StreamNotFound,
}
#[derive(Debug)]
pub struct MpegTsError {
    pub value: MpegTsError,
}

impl From<BytesReadError> for MpegTsError {
    fn from(error: BytesReadError) -> Self {
        MpegTsError {
            value: MpegTsError::BytesReadError(error),
        }
    }
}

impl From<BytesWriteError> for MpegTsError {
    fn from(error: BytesWriteError) -> Self {
        MpegTsError {
            value: MpegTsError::BytesWriteError(error),
        }
    }
}

impl From<Error> for MpegTsError {
    fn from(error: Error) -> Self {
        MpegTsError {
            value: MpegTsError::IOError(error),
        }
    }
}

impl fmt::Display for MpegTsError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.value, f)
    }
}

impl Fail for MpegTsError {
    fn cause(&self) -> Option<&dyn Fail> {
        self.value.cause()
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.value.backtrace()
    }
}
