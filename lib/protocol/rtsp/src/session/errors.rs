use {
    vcp_media_rtp::errors::{PackerError, UnPackerError},
    vcp_media_common::bytesio::bytes_errors::BytesReadError,
    vcp_media_common::bytesio::{bytes_errors::BytesWriteError, bytesio_errors::BytesIOError},
    // crate::common::errors::AuthError,
    thiserror::Error,
    std::str::Utf8Error,
    // streamhub::errors::ChannelError,
    tokio::sync::oneshot::error::RecvError,
};
use vcp_media_sdp::errors::SdpError;



#[derive(Debug, Error)]
pub enum RtspSessionError {
    #[error("net io error: {}", _0)]
    BytesIOError(#[from] BytesIOError),
    #[error("bytes read error: {}", _0)]
    BytesReadError(#[from] BytesReadError),
    #[error("bytes write error: {}", _0)]
    BytesWriteError(#[from] BytesWriteError),
    #[error("Utf8Error: {}", _0)]
    Utf8Error(#[from] Utf8Error),
    #[error("UnPackerError: {}", _0)]
    UnPackerError(#[from] UnPackerError),
    #[error("stream hub event send error")]
    StreamHubEventSendErr,
    #[error("cannot receive frame data from stream hub")]
    CannotReceiveFrameData,
    #[error("pack error: {}", _0)]
    PackerError(#[from] PackerError),
    // #[error("event execute error: {}", _0)]
    // ChannelError(#[from] ChannelError),
    #[error("tokio: oneshot receiver err: {}", _0)]
    RecvError(#[from] RecvError),
    // #[error("auth err: {}", _0)]
    // AuthError(#[from] AuthError),
    #[error("Channel receive error")]
    ChannelRecvError,

    #[error("SdpError: {}", _0)]
    SdpParseError(#[from] SdpError),

    #[error("RecordRangeError")]
    RecordRangeError,
}

