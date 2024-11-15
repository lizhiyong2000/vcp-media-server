use {
    thiserror::Error,
    vcp_media_common::bytesio::bytes_errors::{BytesReadError, BytesWriteError},
    std::fmt,
};


#[derive(Debug, Error)]
pub enum ControlMessagesError {
    //Amf0WriteError(Amf0WriteError),
    #[error("bytes write error: {}", _0)]
    BytesWriteError(#[from] BytesWriteError),
}




#[derive(Debug, Error)]
pub enum ProtocolControlMessageReaderError {
    #[error("bytes read error: {}", _0)]
    BytesReadError(#[from] BytesReadError),
}

