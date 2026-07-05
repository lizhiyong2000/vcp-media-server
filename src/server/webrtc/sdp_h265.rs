use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use std::result::Result as StdResult;
use webrtc::sdp::description::common::Attribute;
use webrtc::sdp::description::media::MediaDescription;
use webrtc::sdp::description::session::SessionDescription;
use tracing::info;

pub fn build_h265_sdp_fmtp(vps: &[u8], sps: &[u8], pps: &[u8]) -> String {
    let vps_b64 = STANDARD.encode(vps);
    let sps_b64 = STANDARD.encode(sps);
    let pps_b64 = STANDARD.encode(pps);
    format!(
        "sprop-vps={vps_b64};sprop-sps={sps_b64};sprop-pps={pps_b64}"
    )
}

pub fn h265_payload_type_from_sdp(sdp: &str) -> Option<u8> {
    let sdp: StdResult<SessionDescription, _> = SessionDescription::try_from(sdp.to_string());
    if let Ok(sdp) = sdp {
        for md in &sdp.media_descriptions {
            for attr in &md.attributes {
                if attr.key == "rtpmap" {
                    if let Some(value) = &attr.value {
                        let parts: Vec<&str> = value.split_whitespace().collect();
                        if parts.len() >= 2 {
                            let codec = parts[1].to_ascii_lowercase();
                            if codec.starts_with("h265/") || codec.starts_with("hevc/") {
                                if let Ok(pt) = parts[0].parse::<u8>() {
                                    return Some(pt);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn find_video_media_description(sdp: &SessionDescription) -> Option<&MediaDescription> {
    sdp.media_descriptions.iter().find(|md| md.media_name.media == "video")
}

fn find_video_media_description_mut(sdp: &mut SessionDescription) -> Option<&mut MediaDescription> {
    sdp.media_descriptions.iter_mut().find(|md| md.media_name.media == "video")
}

fn find_fmtp_attr_index(md: &MediaDescription, payload_type: u8) -> Option<usize> {
    md.attributes.iter().position(|attr| {
        attr.key == "fmtp" && attr.value.as_ref().map_or(false, |v| v.starts_with(&format!("{payload_type} ")))
    })
}

fn find_rtpmap_attr_index(md: &MediaDescription, payload_type: u8) -> Option<usize> {
    md.attributes.iter().position(|attr| {
        attr.key == "rtpmap" && attr.value.as_ref().map_or(false, |v| v.starts_with(&format!("{payload_type} ")))
    })
}

pub fn patch_answer_sdp_h265(sdp: &str, vps: &[u8], sps: &[u8], pps: &[u8]) -> String {
    let mut sdp_result: StdResult<SessionDescription, _> = SessionDescription::try_from(sdp.to_string());
    if let Ok(ref mut sdp) = sdp_result {
        if let Some(md) = find_video_media_description_mut(sdp) {
            if let Some(pt) = h265_payload_type_from_sdp_internal(md) {
                let fmtp = build_h265_sdp_fmtp(vps, sps, pps);
                let fmtp_value = format!("{pt} {fmtp}");
                
                if let Some(idx) = find_fmtp_attr_index(md, pt) {
                    md.attributes[idx].value = Some(fmtp_value);
                } else if let Some(rtpmap_idx) = find_rtpmap_attr_index(md, pt) {
                    md.attributes.insert(rtpmap_idx + 1, Attribute {
                        key: "fmtp".to_string(),
                        value: Some(fmtp_value),
                    });
                }
                
                info!(
                    "[WebRTC] Patched play answer SDP H265 fmtp pt={} vps={}B sps={}B pps={}B",
                    pt,
                    vps.len(),
                    sps.len(),
                    pps.len()
                );
            }
        }
    }
    
    match sdp_result {
        Ok(s) => s.marshal(),
        Err(_) => sdp.to_string(),
    }
}

fn h265_payload_type_from_sdp_internal(md: &MediaDescription) -> Option<u8> {
    for attr in &md.attributes {
        if attr.key == "rtpmap" {
            if let Some(value) = &attr.value {
                let parts: Vec<&str> = value.split_whitespace().collect();
                if parts.len() >= 2 {
                    let codec = parts[1].to_ascii_lowercase();
                    if codec.starts_with("h265/") || codec.starts_with("hevc/") {
                        if let Ok(pt) = parts[0].parse::<u8>() {
                            return Some(pt);
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_h265_sdp_fmtp_generates_correct_format() {
        let vps = vec![0x40, 0x01, 0x0c, 0x01];
        let sps = vec![0x42, 0x01, 0x01, 0x60];
        let pps = vec![0x44, 0x01, 0xc0, 0x73];

        let fmtp = build_h265_sdp_fmtp(&vps, &sps, &pps);
        
        assert!(fmtp.contains("sprop-vps="));
        assert!(fmtp.contains("sprop-sps="));
        assert!(fmtp.contains("sprop-pps="));
    }

    #[test]
    fn patch_answer_sdp_h265_injects_sprop_params() {
        let vps = vec![0x40, 0x01, 0x0c, 0x01];
        let sps = vec![0x42, 0x01, 0x01, 0x60];
        let pps = vec![0x44, 0x01, 0xc0, 0x73];
        let sdp = "v=0
o=- 0 0 IN IP4 127.0.0.1
s=-
t=0 0
m=video 9 UDP/TLS/RTP/SAVPF 127
a=rtpmap:127 H265/90000
";

        let patched = patch_answer_sdp_h265(sdp, &vps, &sps, &pps);

        assert!(patched.contains("a=fmtp:127"));
        assert!(patched.contains("sprop-vps="));
        assert!(patched.contains("sprop-sps="));
        assert!(patched.contains("sprop-pps="));
    }

    #[test]
    fn h265_payload_type_from_sdp_detects_h265() {
        let sdp = "v=0
o=- 0 0 IN IP4 127.0.0.1
s=-
t=0 0
m=video 9 UDP/TLS/RTP/SAVPF 127
a=rtpmap:127 H265/90000
";
        assert_eq!(h265_payload_type_from_sdp(sdp), Some(127));
    }

    #[test]
    fn h265_payload_type_from_sdp_detects_hevc() {
        let sdp = "v=0
o=- 0 0 IN IP4 127.0.0.1
s=-
t=0 0
m=video 9 UDP/TLS/RTP/SAVPF 126
a=rtpmap:126 HEVC/90000
";
        assert_eq!(h265_payload_type_from_sdp(sdp), Some(126));
    }

    #[test]
    fn h265_payload_type_from_sdp_returns_none_for_h264() {
        let sdp = "v=0
o=- 0 0 IN IP4 127.0.0.1
s=-
t=0 0
m=video 9 UDP/TLS/RTP/SAVPF 96
a=rtpmap:96 H264/90000
";
        assert_eq!(h265_payload_type_from_sdp(sdp), None);
    }

    #[test]
    fn h265_payload_type_from_sdp_returns_none_for_empty_sdp() {
        let sdp = "v=0\r\n";
        assert_eq!(h265_payload_type_from_sdp(sdp), None);
    }

    #[test]
    fn patch_answer_sdp_h265_replaces_existing_fmtp() {
        let vps = vec![0x40, 0x01, 0x0c, 0x01];
        let sps = vec![0x42, 0x01, 0x01, 0x60];
        let pps = vec![0x44, 0x01, 0xc0, 0x73];
        let sdp = "v=0
o=- 0 0 IN IP4 127.0.0.1
s=-
t=0 0
m=video 9 UDP/TLS/RTP/SAVPF 127
a=rtpmap:127 H265/90000
a=fmtp:127 foo=bar
";

        let patched = patch_answer_sdp_h265(sdp, &vps, &sps, &pps);

        assert!(patched.contains("sprop-vps="));
        assert!(!patched.contains("foo=bar"));
    }

    #[test]
    fn patch_answer_sdp_h265_with_hevc_mime_type() {
        let vps = vec![0x40, 0x01, 0x0c, 0x01];
        let sps = vec![0x42, 0x01, 0x01, 0x60];
        let pps = vec![0x44, 0x01, 0xc0, 0x73];
        let sdp = "v=0
o=- 0 0 IN IP4 127.0.0.1
s=-
t=0 0
m=video 9 UDP/TLS/RTP/SAVPF 126
a=rtpmap:126 HEVC/90000
";

        let patched = patch_answer_sdp_h265(sdp, &vps, &sps, &pps);

        assert!(patched.contains("a=fmtp:126"));
        assert!(patched.contains("sprop-vps="));
        assert!(patched.contains("sprop-sps="));
        assert!(patched.contains("sprop-pps="));
    }

    #[test]
    fn patch_answer_sdp_h265_preserves_unchanged_when_no_h265() {
        let vps = vec![0x40, 0x01, 0x0c, 0x01];
        let sps = vec![0x42, 0x01, 0x01, 0x60];
        let pps = vec![0x44, 0x01, 0xc0, 0x73];
        let sdp = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 H264/90000\r\n";

        let patched = patch_answer_sdp_h265(sdp, &vps, &sps, &pps);

        assert_eq!(patched, sdp);
    }
}