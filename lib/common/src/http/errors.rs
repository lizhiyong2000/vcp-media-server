use std::fmt;
use failure::{Backtrace, Fail};
use crate::bytesio::bytes_errors::{BytesReadError, BytesWriteError};

#[derive(Debug)]
pub struct HttpError {
    pub value: HttpErrorValue,
}

#[derive(Debug, Fail)]
pub enum HttpErrorValue {
    // #[fail(display = "the uri's scheme is not correct:{}", _0)]
    // UnknownUriSchemeError(String),
    #[fail(display = "the uri's scheme is not correct:{}", _0)]
    RequestUnknownUriSchemeError(String),

    #[fail(display = "the uri's scheme has incorrect prefix:{}", _0)]
    RequestUriSchemePrefixError(String),

    #[fail(display = "the uri's has empty path:{}.", _0)]
    RequestUriPathEmptyError(String),

    #[fail(display = "the http request has no request line.")]
    RequestLineNotFoundError,

    #[fail(display = "the http request has no uri found.")]
    RequestUriNotFoundError,

    #[fail(display = "the http request has incorrect content length.")]
    RequestContentLengthError,

    #[fail(display = "the http request has incorrect content length.")]
    ResponseHeadersError,
}


impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.value, f)
    }
}

impl Fail for HttpError {
    fn cause(&self) -> Option<&dyn Fail> {
        self.value.cause()
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.value.backtrace()
    }
}


impl From<HttpErrorValue> for HttpError {
    fn from(kind: HttpErrorValue) -> HttpError {
        HttpError { value: kind }
    }
}
