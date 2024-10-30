use {
    crate::{
        protocol_control_messages::errors::ProtocolControlMessageReaderError,
        user_control_messages::errors::EventMessagesError,
    },
    vcp_media_common::bytesio::bytes_errors::BytesReadError,
    thiserror::Error,
    std::fmt,
    vcp_media_flv::amf0::errors::Amf0ReadError,
};

#[derive(Debug, Error)]
pub enum MessageError {
    #[error("bytes read error: {}", _0)]
    BytesReadError(#[from] BytesReadError),
    #[error("unknow read state")]
    UnknowReadState,
    #[error("amf0 read error: {}", _0)]
    Amf0ReadError(#[from] Amf0ReadError),
    #[error("unknown message type")]
    UnknowMessageType,
    #[error("protocol control message read error: {}", _0)]
    ProtocolControlMessageReaderError(#[from] ProtocolControlMessageReaderError),
    #[error("user control message read error: {}", _0)]
    EventMessagesError(#[from] EventMessagesError),
}
