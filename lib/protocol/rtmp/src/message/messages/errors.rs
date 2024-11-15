use crate::message::protocol_control_messages::errors::ProtocolControlMessageReaderError;
use crate::message::user_control_messages::errors::EventMessagesError;

use vcp_media_common::bytesio::bytes_errors::BytesReadError;
use vcp_media_flv::amf0::errors::Amf0ReadError;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MessageError {
    #[error("bytes read error: {}", _0)]
    BytesReadError(#[from] BytesReadError),
    #[error("unknow read state")]
    UnknowReadState,
    #[error("amf0 read error: {}", _0)]
    Amf0ReadError(#[from] Amf0ReadError),
    #[error("unknown session type")]
    UnknowMessageType,
    #[error("protocol control session read error: {}", _0)]
    ProtocolControlMessageReaderError(#[from] ProtocolControlMessageReaderError),
    #[error("user control session read error: {}", _0)]
    EventMessagesError(#[from] EventMessagesError),
}
