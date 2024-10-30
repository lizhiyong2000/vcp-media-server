use super::bytesio_errors::BytesIOError;
use std::io;
// use tokio::time::Elapsed;

use thiserror::Error;
use std::fmt;

#[derive(Debug, Error)]
pub enum BytesReadError {
    #[error("not enough bytes to read")]
    NotEnoughBytes,
    #[error("empty stream")]
    EmptyStream,
    #[error("io error: {}", _0)]
    IO(#[from] io::Error),
    #[error("index out of range")]
    IndexOutofRange,
    #[error("bytesio read error: {}", _0)]
    BytesIOError(#[from] BytesIOError),
    // #[error("elapsed: {}", _0)]
    // TimeoutError(#[from] Elapsed),
}



#[derive(Debug, Error)]
pub enum BytesWriteError {
    #[error("io error")]
    IO(#[from] io::Error),
    #[error("bytes io error: {}", _0)]
    BytesIOError(#[from] BytesIOError),
    #[error("write time out")]
    Timeout,
    #[error("outof index")]
    OutofIndex,
}




