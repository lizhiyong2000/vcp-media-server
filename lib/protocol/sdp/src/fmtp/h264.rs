use crate::errors::SdpError;
use base64::{engine::general_purpose, Engine as _};
use bytes::{BufMut, BytesMut};
use vcp_media_common::{Marshal, Unmarshal};
// pub trait Fmtp: TMsgConverter {}

#[derive(Debug, Clone, Default)]
pub struct H264Fmtp {
    pub payload_type: u16,
    pub packetization_mode: u8,
    pub profile_level_id: BytesMut,
    pub sps: BytesMut,
    pub pps: BytesMut,
}

// a=fmtp:96 packetization-mode=1; sprop-parameter-sets=Z2QAFqyyAUBf8uAiAAADAAIAAAMAPB4sXJA=,aOvDyyLA; profile-level-id=640016
impl Unmarshal<&str, Result<Self, SdpError>> for H264Fmtp {
    fn unmarshal(raw_data: &str) -> Result<Self, SdpError> {
        let mut h264_fmtp = H264Fmtp::default();
        let eles: Vec<&str> = raw_data.splitn(2, ' ').collect();
        if eles.len() < 2 {
            log::warn!("H264FmtpSdp parse err: {}", raw_data);
            return Err(SdpError::from(SdpError::SdpFormatParametersError));
        }

        if let Ok(payload_type) = eles[0].parse::<u16>() {
            h264_fmtp.payload_type = payload_type;
        }else{
            log::warn!("H264FmtpSdp parse err: {}", raw_data);
            return Err(SdpError::from(SdpError::SdpPayloadTypeError));
        }

        let parameters: Vec<&str> = eles[1].split(';').collect();
        for parameter in parameters {
            let kv: Vec<&str> = parameter.trim().splitn(2, '=').collect();
            if kv.len() < 2 {
                log::warn!("H264FmtpSdp parse key=value err: {}", parameter);
                continue;
            }
            match kv[0] {
                "packetization-mode" => {
                    if let Ok(packetization_mode) = kv[1].parse::<u8>() {
                        h264_fmtp.packetization_mode = packetization_mode;
                    }
                }
                "sprop-parameter-sets" => {
                    let spspps: Vec<&str> = kv[1].split(',').collect();
                    if spspps.len() < 2 {
                        log::warn!("H264FmtpSdp parse sprop-parameter-sets err: {}", kv[1]);
                        continue;
                    }

                    let sps = general_purpose::STANDARD.decode(spspps[0]);
                    match sps {
                        Ok(sps) => {
                            h264_fmtp.sps.put(&sps[..]);
                        }
                        _ => {
                            log::warn!("H264FmtpSdp parse sps err: {}", spspps[0]);
                        }

                    }

                    let pps = general_purpose::STANDARD.decode(spspps[1]);

                    match pps {
                        Ok(pps) => {
                            h264_fmtp.pps.put(&pps[..]);
                        }
                        _ => {
                            log::warn!("H264FmtpSdp parse key=value err: {}", spspps[1]);
                        }

                    }

                }
                "profile-level-id" => {
                    h264_fmtp.profile_level_id = kv[1].into();
                }
                _ => {
                    log::info!("not parsed: {}", kv[0])
                }
            }
        }

        Ok(h264_fmtp)
    }
}

impl Marshal<String> for H264Fmtp {
    // a=fmtp:96 packetization-mode=1; sprop-parameter-sets=Z2QAFqyyAUBf8uAiAAADAAIAAAMAPB4sXJA=,aOvDyyLA; profile-level-id=640016
    fn marshal(&self) -> String {
        let sps_str = general_purpose::STANDARD.encode(&self.sps);
        let pps_str = general_purpose::STANDARD.encode(&self.pps);
        let profile_level_id_str = String::from_utf8(self.profile_level_id.to_vec()).unwrap();

        let h264_fmtp = format!(
            "{} packetization-mode={}; sprop-parameter-sets={},{}; profile-level-id={}",
            self.payload_type, self.packetization_mode, sps_str, pps_str, profile_level_id_str
        );

        format!("{h264_fmtp}\r\n")
    }
}

#[cfg(test)]
mod tests {
    use super::H264Fmtp;
    use vcp_media_common::Marshal;
    use vcp_media_common::Unmarshal;
    // use vcp_media_rtsp::rtsp_utils;

    #[test]
    fn test_parse_h264fmtpsdp() {
        let parser =  H264Fmtp::unmarshal("96 packetization-mode=1; sprop-parameter-sets=Z2QAFqyyAUBf8uAiAAADAAIAAAMAPB4sXJA=,aOvDyyLA; profile-level-id=640016").unwrap();

        println!(" parser: {parser:?}");

        assert_eq!(parser.packetization_mode, 1);
        assert_eq!(parser.profile_level_id, "640016");
        // assert_eq!(parser.sps, "Z2QAFqyyAUBf8uAiAAADAAIAAAMAPB4sXJA=");
        // assert_eq!(parser.pps, "aOvDyyLA");
        //"96 packetization-mode=1; sprop-parameter-sets=Z2QAFqyyAUBf8uAiAAADAAIAAAMAPB4sXJA=,aOvDyyLA; profile-level-id=640016"

        print!("264 parser: {}", parser.marshal());

        let parser2 = H264Fmtp::unmarshal("96 packetization-mode=1;\nsprop-parameter-sets=Z2QAFqyyAUBf8uAiAAADAAIAAAMAPB4sXJA=,aOvDyyLA;\nprofile-level-id=640016").unwrap();

        println!(" parser: {parser2:?}");

        assert_eq!(parser2.packetization_mode, 1);
        assert_eq!(parser2.profile_level_id, "640016");
        // assert_eq!(parser2.sps, "Z2QAFqyyAUBf8uAiAAADAAIAAAMAPB4sXJA=");
        // assert_eq!(parser2.pps, "aOvDyyLA");

        print!("264 parser2: {}", parser2.marshal());
    }

}
