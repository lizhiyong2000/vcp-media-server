use std::fmt;
use failure::{Backtrace, Fail};

#[derive(Debug)]
pub struct RtspError {
    pub value: RtspErrorValue,
}

#[derive(Debug, Fail)]
pub enum RtspErrorValue {
    #[fail(display = "rtsp range parse error.")]
    RtspRangeError,
    #[fail(display = "rtsp transport parse error.")]
    RtspTransportError,
}


impl fmt::Display for RtspError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.value, f)
    }
}

impl Fail for RtspError {
    fn cause(&self) -> Option<&dyn Fail> {
        self.value.cause()
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.value.backtrace()
    }
}


impl From<RtspErrorValue> for RtspError {
    fn from(kind: RtspErrorValue) -> RtspError {
        RtspError { value: kind }
    }
}

