mod h264;
mod h265;
mod mpeg4;

use vcp_media_common::{Marshal, Unmarshal};
use base64::{engine::general_purpose, Engine as _};
use bytes::{BufMut, BytesMut};

use h264::H264Fmtp;
use h265::H265Fmtp;
use mpeg4::Mpeg4Fmtp;
use crate::errors::{SdpError, SdpErrorValue};

#[derive(Debug, Clone)]
pub enum Fmtp {
    H264(H264Fmtp),
    H265(H265Fmtp),
    Mpeg4(Mpeg4Fmtp),
}

impl Fmtp {
    pub fn new(codec: &str, raw_data: &str) -> Result<Fmtp, SdpError> {
        match codec.to_lowercase().as_str() {
            "h264" => {
                let h264_fmtp = H264Fmtp::unmarshal(raw_data)?;
                    return Ok(Fmtp::H264(h264_fmtp));

            }
            "h265" => {
                let h265_fmtp = H265Fmtp::unmarshal(raw_data)?;
                    return Ok(Fmtp::H265(h265_fmtp));

            }
            "mpeg4-generic" => {
                let mpeg4_fmtp = Mpeg4Fmtp::unmarshal(raw_data)?;
                    return Ok(Fmtp::Mpeg4(mpeg4_fmtp));

            }
            _ => {
                return Err(SdpError::from(SdpErrorValue::SdpUnknownCodecError(codec.to_string())));
            }
        }
    }

    pub fn marshal(&self) -> String {
        match self {
            Fmtp::H264(h264fmtp) => h264fmtp.marshal(),
            Fmtp::H265(h265fmtp) => h265fmtp.marshal(),
            Fmtp::Mpeg4(mpeg4fmtp) => mpeg4fmtp.marshal(),
        }
    }
}

