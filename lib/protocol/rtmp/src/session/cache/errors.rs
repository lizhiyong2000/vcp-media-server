use thiserror::Error;
use {
    crate::message::chunk::errors::PackError,
    vcp_media_common::bytesio::bytes_errors::BytesReadError,
    vcp_media_h264::decoder::errors::H264Error,
    std::fmt,
    vcp_media_flv::amf0::errors::Amf0WriteError,
    vcp_media_flv::errors::{FlvDemuxerError, MpegError},
};

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cache tag parse error")]
    DemuxerError(#[from] FlvDemuxerError),
    #[error("mpeg aac error")]
    MpegError(#[from] MpegError),
    // #[error("mpeg avc error")]
    // MpegAvcError(Mpeg4AvcHevcError),
    #[error("pack error")]
    PackError(#[from] PackError),
    #[error("read bytes error")]
    BytesReadError(#[from] BytesReadError),
    #[error("h264 error")]
    H264Error(#[from] H264Error),
}


#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("metadata tag parse error")]
    DemuxerError(#[from] FlvDemuxerError),
    #[error("pack error")]
    PackError(#[from] PackError),
    #[error("amf write error")]
    Amf0WriteError(#[from] Amf0WriteError),
}
