use super::bytes_errors::BytesReadError;
use super::bytes_errors::BytesWriteError;
use thiserror::Error;
use std::fmt;

#[derive(Debug, Error)]
pub enum BitError {
    #[error("bytes read error")]
    BytesReadError(#[from] BytesReadError),
    #[error("bytes write error")]
    BytesWriteError(#[from] BytesWriteError),
    #[error("the size is bigger than 64")]
    TooBig,
    #[error("cannot write the whole 8 bits")]
    CannotWrite8Bit,
    #[error("cannot read byte")]
    CannotReadByte,
}


// impl From<Elapsed> for NetIOError {
//     fn from(error: Elapsed) -> Self {
//         NetIOError {
//             value: NetIOError::TimeoutError(error),
//         }
//     }
// }


