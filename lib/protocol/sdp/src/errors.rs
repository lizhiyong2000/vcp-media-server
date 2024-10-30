use std::fmt;
use vcp_media_common::bytesio::bytes_errors::BytesReadError;
use vcp_media_common::bytesio::bytes_errors::BytesWriteError;
use failure::{Backtrace, Fail};
use vcp_media_common::bytesio::bits_errors::BitError;

#[derive(Debug)]
pub struct SdpError {
    pub value: SdpErrorValue,
}

#[derive(Debug, Fail)]
pub enum SdpErrorValue {
    // #[fail(display = "bits error:{}", _0)]
    // BitError(#[cause] BitError),
    // #[fail(display = "h264 error:{}", _0)]
    // H264Error(#[cause] H264Error),
    #[fail(display = "the session origin is not correct")]
    SessionOriginError,
    #[fail(display = "should not come here")]
    ShouldNotComeHere,
    #[fail(display = "the sps nal unit type is not correct")]
    SPSNalunitTypeNotCorrect,
    #[fail(display = "not supported sampling frequency")]
    NotSupportedSamplingFrequency,

    #[fail(display = "the sps nal unit type is not correct")]
    SdpFormatParametersError,

    #[fail(display = "the sps nal unit type is not correct")]
    SdpPayloadTypeError,

    #[fail(display = "the sdp codec {} not supported", _0)]
    SdpUnknownCodecError(String),
}


impl fmt::Display for SdpError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.value, f)
    }
}

impl Fail for SdpError {
    fn cause(&self) -> Option<&dyn Fail> {
        self.value.cause()
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.value.backtrace()
    }
}

impl From<SdpErrorValue> for SdpError {
    fn from(kind: SdpErrorValue) -> SdpError {
        SdpError { value: kind }
    }
}

// impl From<BytesReadError> for SdpError {
//     fn from(error: BytesReadError) -> Self {
//         RtcpError {
//             value: RtcpErrorValue::BytesReadError(error),
//         }
//     }
// }
//
// impl From<BytesWriteError> for SdpError {
//     fn from(error: BytesWriteError) -> Self {
//         RtcpError {
//             value: RtcpErrorValue::BytesWriteError(error),
//         }
//     }
// }


