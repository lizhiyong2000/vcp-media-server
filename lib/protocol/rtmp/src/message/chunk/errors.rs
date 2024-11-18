use {
    thiserror::Error,
    vcp_media_common::bytesio::bytes_errors::{BytesReadError, BytesWriteError}
    ,
};

#[derive(Debug, Error)]
pub enum UnpackError {
    #[error("bytes read error: {}", _0)]
    BytesReadError(#[from] BytesReadError),
    #[error("unknow read state")]
    UnknowReadState,
    #[error("empty chunks")]
    EmptyChunks,
    //IO(io::Error),
    #[error("cannot parse")]
    CannotParse,
}



#[derive(Debug, Error)]
pub enum PackError {
    #[error("not exist header")]
    NotExistHeader,
    #[error("unknow read state")]
    UnknowReadState,
    #[error("bytes writer error: {}", _0)]
    BytesWriteError(#[from] BytesWriteError),
}


