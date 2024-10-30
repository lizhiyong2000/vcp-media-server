use std::fmt;
use thiserror::Error;
use crate::bytesio::bytes_errors::{BytesReadError, BytesWriteError};

#[derive(Debug, Error)]
pub enum HttpError {
    // #[error("the uri's scheme is not correct:{}", _0)]
    // UnknownUriSchemeError(String),
    #[error("the uri's scheme is not correct:{}", _0)]
    RequestUnknownUriSchemeError(String),

    #[error("the uri's scheme has incorrect prefix:{}", _0)]
    RequestUriSchemePrefixError(String),

    #[error("the uri's has empty path:{}.", _0)]
    RequestUriPathEmptyError(String),

    #[error("the http request has no request line.")]
    RequestLineNotFoundError,

    #[error("the http request has no uri found.")]
    RequestUriNotFoundError,

    #[error("the http request has incorrect content length.")]
    RequestContentLengthError,

    #[error("the http request has incorrect content length.")]
    ResponseHeadersError,
}


// impl fmt::Display for HttpError {
//     fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
//         fmt::Display::fmt(&self.value, f)
//     }
// }


