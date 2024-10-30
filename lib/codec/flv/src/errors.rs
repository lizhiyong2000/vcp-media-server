use {
    vcp_media_common::bytesio::bits_errors::BitError,
    vcp_media_common::bytesio::bytes_errors::{BytesReadError, BytesWriteError},
    thiserror::Error,
    vcp_media_h264::errors::H264Error,
    std::fmt,
};

#[derive(Debug, Error)]
pub enum TagParseError {
    #[error("bytes read error")]
    BytesReadError(#[from] BytesReadError),
    #[error("tag data length error")]
    TagDataLength,
    #[error("unknow tag type error")]
    UnknownTagType,
}


#[derive(Debug, Error)]
pub enum FlvMuxerError {
    // #[error("server error")]
    // Error,
    #[error("bytes write error")]
    BytesWriteError(#[from] BytesWriteError),
}


#[derive(Debug, Error)]
pub enum FlvDemuxerError {
    // #[error("server error")]
    // Error,
    #[error("bytes write error:{}", _0)]
    BytesWriteError(#[from] BytesWriteError),
    #[error("bytes read error:{}", _0)]
    BytesReadError(#[from] BytesReadError),
    // #[error("mpeg avc error:{}", _0)]
    // MpegAvcError(#[from] MpegError),
    #[error("mpeg aac error:{}", _0)]
    MpegError(#[from] MpegError),
}


#[derive(Debug, Error)]
pub enum MpegError {
    #[error("bytes read error:{}", _0)]
    BytesReadError(#[from] BytesReadError),
    #[error("bytes write error:{}", _0)]
    BytesWriteError(#[from] BytesWriteError),
    #[error("bits error:{}", _0)]
    BitError(#[from] BitError),
    #[error("h264 error:{}", _0)]
    H264Error(#[from] H264Error),
    #[error("there is not enough bits to read")]
    NotEnoughBitsToRead,
    #[error("should not come here")]
    ShouldNotComeHere,
    #[error("the sps nal unit type is not correct")]
    SPSNalunitTypeNotCorrect,
    #[error("not supported sampling frequency")]
    NotSupportedSamplingFrequency,
}




#[derive(Debug, Error)]
pub enum BitVecError {
    #[error("not enough bits left")]
    NotEnoughBits,
}

