use std::fmt;
use thiserror::Error;



#[derive(Debug, Error)]
pub enum RtspError {
    #[error("rtsp range parse error.")]
    RtspRangeError,
    #[error("rtsp transport parse error.")]
    RtspTransportError,
}



