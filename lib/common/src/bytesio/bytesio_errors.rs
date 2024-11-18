use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BytesIOError {
    #[error("not enough bytes")]
    NotEnoughBytes,
    #[error("empty stream")]
    EmptyStream,
    #[error("io error")]
    IOError(#[from] io::Error),
    #[error("time out error")]
    TimeoutError(#[from] tokio::time::error::Elapsed),
    #[error("none return")]
    NoneReturn,
}
// #[derive(Debug)]
// pub struct BytesIOError {
//     pub value: BytesIOError,
// }
//
// impl From<BytesIOError> for BytesIOError {
//     fn from(val: BytesIOError) -> Self {
//         BytesIOError { value: val }
//     }
// }
//
// impl From<io::Error> for BytesIOError {
//     fn from(error: io::Error) -> Self {
//         BytesIOError {
//             value: BytesIOError::IOError(error),
//         }
//     }
// }

// impl From<Elapsed> for NetIOError {
//     fn from(error: Elapsed) -> Self {
//         NetIOError {
//             value: NetIOError::TimeoutError(error),
//         }
//     }
// }

// impl fmt::Display for BytesIOError {
//     fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
//         fmt::Display::fmt(&self.value, f)
//     }
// }

