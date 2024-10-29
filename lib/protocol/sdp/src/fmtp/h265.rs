use vcp_media_common::{Marshal, Unmarshal};
use base64::{engine::general_purpose, Engine as _};
use bytes::{BufMut, BytesMut};


#[derive(Debug, Clone, Default)]
pub struct H265Fmtp {
    pub payload_type: u16,
    pub vps: BytesMut,
    pub sps: BytesMut,
    pub pps: BytesMut,
}



impl Unmarshal<&str, Option<H265Fmtp>> for H265Fmtp {
    //"a=fmtp:96 sprop-vps=QAEMAf//AWAAAAMAkAAAAwAAAwA/ugJA; sprop-sps=QgEBAWAAAAMAkAAAAwAAAwA/oAUCAXHy5bpKTC8BAQAAAwABAAADAA8I; sprop-pps=RAHAc8GJ"
    fn unmarshal(raw_data: &str) -> Option<Self> {
        let mut h265_fmtp = H265Fmtp::default();
        let eles: Vec<&str> = raw_data.splitn(2, ' ').collect();
        if eles.len() < 2 {
            log::warn!("H265FmtpSdp parse err: {}", raw_data);
            return None;
        }

        if let Ok(payload_type) = eles[0].parse::<u16>() {
            h265_fmtp.payload_type = payload_type;
        }

        let parameters: Vec<&str> = eles[1].split(';').collect();
        for parameter in parameters {
            let kv: Vec<&str> = parameter.trim().splitn(2, '=').collect();
            if kv.len() < 2 {
                log::warn!("H265FmtpSdp parse key=value err: {}", parameter);
                continue;
            }

            match kv[0] {
                "sprop-vps" => {
                    h265_fmtp.vps = kv[1].into();
                }
                "sprop-sps" => {
                    h265_fmtp.sps = kv[1].into();
                }
                "sprop-pps" => {
                    h265_fmtp.pps = kv[1].into();
                }
                _ => {
                    log::info!("not parsed: {}", kv[0])
                }
            }
        }

        Some(h265_fmtp)
    }
}

impl Marshal<String> for H265Fmtp {
    //"a=fmtp:96 sprop-vps=QAEMAf//AWAAAAMAkAAAAwAAAwA/ugJA; sprop-sps=QgEBAWAAAAMAkAAAAwAAAwA/oAUCAXHy5bpKTC8BAQAAAwABAAADAA8I; sprop-pps=RAHAc8GJ"
    fn marshal(&self) -> String {
        let sps_str = String::from_utf8(self.sps.to_vec()).unwrap();
        let pps_str = String::from_utf8(self.pps.to_vec()).unwrap();
        let vps_str = String::from_utf8(self.vps.to_vec()).unwrap();

        let h265_fmtp = format!(
            "{} sprop-vps={}; sprop-sps={}; sprop-pps={}",
            self.payload_type, vps_str, sps_str, pps_str
        );

        format!("{h265_fmtp}\r\n")
    }
}

#[cfg(test)]
mod tests {

    use bytes::BytesMut;

    use super::H265Fmtp;
    use vcp_media_common::Marshal;
    use vcp_media_common::Unmarshal;
    // use vcp_media_rtsp::rtsp_utils;

    #[test]
    fn test_parse_h265fmtpsdp() {
        let parser = H265Fmtp::unmarshal("96 sprop-vps=QAEMAf//AWAAAAMAkAAAAwAAAwA/ugJA; sprop-sps=QgEBAWAAAAMAkAAAAwAAAwA/oAUCAXHy5bpKTC8BAQAAAwABAAADAA8I; sprop-pps=RAHAc8GJ").unwrap();

        println!(" parser: {parser:?}");

        assert_eq!(parser.vps, "QAEMAf//AWAAAAMAkAAAAwAAAwA/ugJA");
        assert_eq!(
            parser.sps,
            "QgEBAWAAAAMAkAAAAwAAAwA/oAUCAXHy5bpKTC8BAQAAAwABAAADAA8I"
        );
        assert_eq!(parser.pps, "RAHAc8GJ");

        print!("265 parser: {}", parser.marshal());
    }

}
