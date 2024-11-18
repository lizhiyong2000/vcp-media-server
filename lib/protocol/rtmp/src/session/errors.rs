use crate::message::chunk::errors::{PackError, UnpackError};
use crate::message::handshake::errors::HandshakeError;
use crate::message::messages::errors::MessageError;
use crate::message::netconnection::errors::NetConnectionError;
use crate::message::netstream::errors::NetStreamError;
use crate::message::protocol_control_messages::errors::ControlMessagesError;
use crate::message::user_control_messages::errors::EventMessagesError;
use thiserror::Error;
use vcp_media_common::bytesio::bytes_errors::BytesWriteError;
use vcp_media_common::bytesio::bytesio_errors::BytesIOError;
use vcp_media_flv::amf0::Amf0WriteError;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("amf0 write error: {}", _0)]
    Amf0WriteError(#[from] Amf0WriteError),
    #[error("bytes write error: {}", _0)]
    BytesWriteError(#[from] BytesWriteError),
    // #[error("timeout error: {}", _0)]
    // TimeoutError(#[from] Elapsed),
    #[error("unpack error: {}", _0)]
    UnPackError(#[from] UnpackError),

    #[error("message error: {}", _0)]
    MessageError(#[from] MessageError),
    #[error("control message error: {}", _0)]
    ControlMessagesError(#[from] ControlMessagesError),
    #[error("net connection error: {}", _0)]
    NetConnectionError(#[from] NetConnectionError),
    #[error("net stream error: {}", _0)]
    NetStreamError(#[from] NetStreamError),

    #[error("event messages error: {}", _0)]
    EventMessagesError(#[from] EventMessagesError),
    #[error("net io error: {}", _0)]
    BytesIOError(#[from] BytesIOError),
    #[error("pack error: {}", _0)]
    PackError(#[from] PackError),
    #[error("handshake error: {}", _0)]
    HandshakeError(#[from] HandshakeError),
    // #[error("cache error name: {}", _0)]
    // CacheError(#[from] CacheError),
    // #[error("tokio: oneshot receiver err: {}", _0)]
    // RecvError(#[from] RecvError),
    // #[error("streamhub channel err: {}", _0)]
    // ChannelError(#[from] StreamHubError),

    #[error("amf0 count not correct error")]
    Amf0ValueCountNotCorrect,
    #[error("amf0 value type not correct error")]
    Amf0ValueTypeNotCorrect,
    #[error("stream hub event send error")]
    StreamHubEventSendErr,
    #[error("none frame data sender error")]
    NoneFrameDataSender,
    #[error("none frame data receiver error")]
    NoneFrameDataReceiver,
    #[error("send frame data error")]
    SendFrameDataErr,
    #[error("subscribe count limit is reached.")]
    SubscribeCountLimitReach,

    #[error("no app name error")]
    NoAppName,
    #[error("no media data can be received now.")]
    NoMediaDataReceived,

    #[error("session is finished.")]
    Finish,
    // #[error("Auth err: {}", _0)]
    // AuthError(#[from] AuthError),
}
