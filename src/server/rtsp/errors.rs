use {
    vcp_media_rtp::errors::{PackerError, UnPackerError},
    vcp_media_common::bytesio::bytes_errors::BytesReadError,
    vcp_media_common::bytesio::{bytes_errors::BytesWriteError, bytesio_errors::BytesIOError},
    crate::common::errors::AuthError,
    failure::{Backtrace, Fail},
    std::fmt,
    std::str::Utf8Error,
    // streamhub::errors::ChannelError,
    tokio::sync::oneshot::error::RecvError,
};
use vcp_media_sdp::errors::SdpError;

#[derive(Debug)]
pub struct RtspSessionError {
    pub value: RtspSessionErrorValue,
}

#[derive(Debug, Fail)]
pub enum RtspSessionErrorValue {
    #[fail(display = "net io error: {}", _0)]
    BytesIOError(#[cause] BytesIOError),
    #[fail(display = "bytes read error: {}", _0)]
    BytesReadError(#[cause] BytesReadError),
    #[fail(display = "bytes write error: {}", _0)]
    BytesWriteError(#[cause] BytesWriteError),
    #[fail(display = "Utf8Error: {}", _0)]
    Utf8Error(#[cause] Utf8Error),
    #[fail(display = "UnPackerError: {}", _0)]
    UnPackerError(#[cause] UnPackerError),
    #[fail(display = "stream hub event send error")]
    StreamHubEventSendErr,
    #[fail(display = "cannot receive frame data from stream hub")]
    CannotReceiveFrameData,
    #[fail(display = "pack error: {}", _0)]
    PackerError(#[cause] PackerError),
    // #[fail(display = "event execute error: {}", _0)]
    // ChannelError(#[cause] ChannelError),
    #[fail(display = "tokio: oneshot receiver err: {}", _0)]
    RecvError(#[cause] RecvError),
    #[fail(display = "auth err: {}", _0)]
    AuthError(#[cause] AuthError),
    #[fail(display = "Channel receive error")]
    ChannelRecvError,

    #[fail(display = "SdpError: {}", _0)]
    SdpParseError(#[cause] SdpError),

    #[fail(display = "RecordRangeError")]
    RecordRangeError,
}


impl From<RtspSessionErrorValue> for RtspSessionError {
    fn from(error: RtspSessionErrorValue) -> Self {
        RtspSessionError {
            value: error,
        }
    }


}

impl From<BytesIOError> for RtspSessionError {
    fn from(error: BytesIOError) -> Self {
        RtspSessionError {
            value: RtspSessionErrorValue::BytesIOError(error),
        }
    }
}

impl From<BytesReadError> for RtspSessionError {
    fn from(error: BytesReadError) -> Self {
        RtspSessionError {
            value: RtspSessionErrorValue::BytesReadError(error),
        }
    }
}

impl From<BytesWriteError> for RtspSessionError {
    fn from(error: BytesWriteError) -> Self {
        RtspSessionError {
            value: RtspSessionErrorValue::BytesWriteError(error),
        }
    }
}

impl From<Utf8Error> for RtspSessionError {
    fn from(error: Utf8Error) -> Self {
        RtspSessionError {
            value: RtspSessionErrorValue::Utf8Error(error),
        }
    }
}

impl From<PackerError> for RtspSessionError {
    fn from(error: PackerError) -> Self {
        RtspSessionError {
            value: RtspSessionErrorValue::PackerError(error),
        }
    }
}

impl From<UnPackerError> for RtspSessionError {
    fn from(error: UnPackerError) -> Self {
        RtspSessionError {
            value: RtspSessionErrorValue::UnPackerError(error),
        }
    }
}

// impl From<ChannelError> for SessionError {
//     fn from(error: ChannelError) -> Self {
//         SessionError {
//             value: SessionErrorValue::ChannelError(error),
//         }
//     }
// }

impl From<RecvError> for RtspSessionError {
    fn from(error: RecvError) -> Self {
        RtspSessionError {
            value: RtspSessionErrorValue::RecvError(error),
        }
    }
}

impl From<AuthError> for RtspSessionError {
    fn from(error: AuthError) -> Self {
        RtspSessionError {
            value: RtspSessionErrorValue::AuthError(error),
        }
    }
}

impl From<SdpError> for RtspSessionError {
    fn from(error: SdpError) -> Self {
        RtspSessionError {
            value: RtspSessionErrorValue::SdpParseError(error),
        }
    }
}




impl fmt::Display for RtspSessionError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.value, f)
    }
}

impl Fail for RtspSessionError {
    fn cause(&self) -> Option<&dyn Fail> {
        self.value.cause()
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.value.backtrace()
    }
}
