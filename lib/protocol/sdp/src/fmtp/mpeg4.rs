use vcp_media_common::{Marshal, Unmarshal};
use base64::{engine::general_purpose, Engine as _};
use bytes::{BufMut, BytesMut};
use crate::errors::{SdpError};

#[derive(Debug, Clone, Default)]
pub struct Mpeg4Fmtp {
    pub payload_type: u16,
    pub asc: BytesMut,
    profile_level_id: BytesMut,
    mode: String,
    size_length: u16,
    index_length: u16,
    index_delta_length: u16,
}

impl Unmarshal<&str, Result<Self, SdpError>> for Mpeg4Fmtp {
    //a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3; config=121056e500
    fn unmarshal(raw_data: &str) -> Result<Self, SdpError> {
        let mut mpeg4_fmtp = Mpeg4Fmtp::default();
        let eles: Vec<&str> = raw_data.splitn(2, ' ').collect();
        if eles.len() < 2 {
            log::warn!("Mpeg4FmtpSdp parse err: {}", raw_data);
            return Err(SdpError::from(SdpError::SdpFormatParametersError));
        }

        if let Ok(payload_type) = eles[0].parse::<u16>() {
            mpeg4_fmtp.payload_type = payload_type;
        }else{
            log::warn!("Mepg4FmtpSdp parse err: {}", raw_data);
            return Err(SdpError::from(SdpError::SdpPayloadTypeError));
        }

        let parameters: Vec<&str> = eles[1].split(';').collect();
        for parameter in parameters {
            let kv: Vec<&str> = parameter.trim().splitn(2, '=').collect();
            if kv.len() < 2 {
                log::warn!("Mpeg4FmtpSdp parse key=value err: {}", parameter);
                continue;
            }
            match kv[0].to_lowercase().as_str() {
                "mode" => {
                    mpeg4_fmtp.mode = kv[1].to_string();
                }
                "config" => {
                    let asc = hex::decode(kv[1]).unwrap();
                    mpeg4_fmtp.asc.put(&asc[..]);
                }
                "profile-level-id" => {
                    mpeg4_fmtp.profile_level_id = kv[1].into();
                }
                "sizelength" => {
                    if let Ok(size_length) = kv[1].parse::<u16>() {
                        mpeg4_fmtp.size_length = size_length;
                    }
                }
                "indexlength" => {
                    if let Ok(index_length) = kv[1].parse::<u16>() {
                        mpeg4_fmtp.index_length = index_length;
                    }
                }
                "indexdeltalength" => {
                    if let Ok(index_delta_length) = kv[1].parse::<u16>() {
                        mpeg4_fmtp.index_delta_length = index_delta_length;
                    }
                }
                _ => {
                    log::info!("not parsed: {}", kv[0])
                }
            }
        }

        Ok(mpeg4_fmtp)
    }
}

impl Marshal<String> for Mpeg4Fmtp {
    //a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3; config=121056e500
    fn marshal(&self) -> String {
        let profile_level_id_str = String::from_utf8(self.profile_level_id.to_vec()).unwrap();
        let asc_str = hex::encode(&self.asc); //String::from_utf8(self.asc.to_vec()).unwrap();

        let mpeg4_fmtp = format!(
            "{} profile-level-id={};mode={};sizelength={};indexlength={};indexdeltalength={}; config={}",
            self.payload_type, profile_level_id_str, self.mode, self.size_length, self.index_length,
            self.index_delta_length,asc_str);

        format!("{mpeg4_fmtp}\r\n")
    }
}

#[cfg(test)]
mod tests {

    use bytes::BytesMut;

    use super::Mpeg4Fmtp;
    use vcp_media_common::Marshal;
    use vcp_media_common::Unmarshal;
    // use vcp_media_rtsp::rtsp_utils;


    #[test]
    fn test_parse_mpeg4fmtpsdp() {
        let parser = Mpeg4Fmtp::unmarshal("97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=23; config=121056e500").unwrap();

        println!(" parser: {parser:?}");
        let en_asc = hex::encode(parser.asc.clone());

        assert_eq!(en_asc, "121056e500");
        assert_eq!(parser.profile_level_id, "1");
        assert_eq!(parser.mode, "AAC-hbr");
        assert_eq!(parser.size_length, 13);
        assert_eq!(parser.index_length, 3);
        assert_eq!(parser.index_delta_length, 23);

        print!("mpeg4 parser: {}", parser.marshal());
    }

}
