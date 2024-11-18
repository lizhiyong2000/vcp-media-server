use thiserror::Error;


#[derive(Debug, Error)]
pub enum SdpError {
    // #[error("bits error:{}", _0)]
    // BitError(#[from] BitError),
    // #[error("h264 error:{}", _0)]
    // H264Error(#[from] H264Error),
    #[error("the session origin is not correct")]
    SessionOriginError,
    #[error("should not come here")]
    ShouldNotComeHere,
    #[error("the sps nal unit type is not correct")]
    SPSNalunitTypeNotCorrect,
    #[error("not supported sampling frequency")]
    NotSupportedSamplingFrequency,

    #[error("the sps nal unit type is not correct")]
    SdpFormatParametersError,

    #[error("the sps nal unit type is not correct")]
    SdpPayloadTypeError,

    #[error("the sdp codec {} not supported", _0)]
    SdpUnknownCodecError(String),
}


// impl From<BytesReadError> for SdpError {
//     fn from(error: BytesReadError) -> Self {
//         RtcpError {
//             value: RtcpError::BytesReadError(error),
//         }
//     }
// }
//
// impl From<BytesWriteError> for SdpError {
//     fn from(error: BytesWriteError) -> Self {
//         RtcpError {
//             value: RtcpError::BytesWriteError(error),
//         }
//     }
// }


