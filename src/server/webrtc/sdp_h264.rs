use base64::Engine;
use std::result::Result as StdResult;
use webrtc::sdp::description::common::Attribute;
use webrtc::sdp::description::media::MediaDescription;
use webrtc::sdp::description::session::SessionDescription;
use tracing::info;

pub fn build_h264_sdp_fmtp(sps: &[u8], pps: &[u8]) -> String {
    let profile = profile_level_id_from_sps(sps).unwrap_or_else(|| "42e01f".to_string());
    let sps_b64 = base64::engine::general_purpose::STANDARD.encode(sps);
    let pps_b64 = base64::engine::general_purpose::STANDARD.encode(pps);
    format!(
        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={profile};sprop-parameter-sets={sps_b64},{pps_b64}"
    )
}

pub fn profile_level_id_from_sps(sps: &[u8]) -> Option<String> {
    if sps.is_empty() {
        return None;
    }
    let start = if sps[0] & 0x1F == 7 { 1 } else { 0 };
    if sps.len() < start + 3 {
        return None;
    }
    Some(format!(
        "{:02x}{:02x}{:02x}",
        sps[start],
        sps[start + 1],
        sps[start + 2]
    ))
}

pub fn h264_payload_type_from_sdp(sdp: &str) -> Option<u8> {
    let sdp: StdResult<SessionDescription, _> = SessionDescription::try_from(sdp.to_string());
    if let Ok(sdp) = sdp {
        for md in &sdp.media_descriptions {
            for attr in &md.attributes {
                if attr.key == "rtpmap" {
                    if let Some(value) = &attr.value {
                        let parts: Vec<&str> = value.split_whitespace().collect();
                        if parts.len() >= 2 && parts[1].to_ascii_lowercase().starts_with("h264/") {
                            if let Ok(pt) = parts[0].parse::<u8>() {
                                return Some(pt);
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

pub fn patch_answer_sdp_h264(sdp: &str, sps: &[u8], pps: &[u8]) -> String {
    let mut sdp_result: StdResult<SessionDescription, _> = SessionDescription::try_from(sdp.to_string());
    if let Ok(ref mut sdp) = sdp_result {
        if let Some(md) = find_video_media_description_mut(sdp) {
            if let Some(pt) = h264_payload_type_from_sdp_internal(md) {
                let fmtp = build_h264_sdp_fmtp(sps, pps);
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
                    "[WebRTC] Patched play answer SDP fmtp pt={} profile={}",
                    pt,
                    profile_level_id_from_sps(sps).unwrap_or_default()
                );
            }
        }
    }
    
    match sdp_result {
        Ok(s) => s.marshal(),
        Err(_) => sdp.to_string(),
    }
}

fn h264_payload_type_from_sdp_internal(md: &MediaDescription) -> Option<u8> {
    for attr in &md.attributes {
        if attr.key == "rtpmap" {
            if let Some(value) = &attr.value {
                let parts: Vec<&str> = value.split_whitespace().collect();
                if parts.len() >= 2 && parts[1].to_ascii_lowercase().starts_with("h264/") {
                    if let Ok(pt) = parts[0].parse::<u8>() {
                        return Some(pt);
                    }
                }
            }
        }
    }
    None
}

pub fn parse_sprop_parameter_sets(sdp: &str) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let sdp: StdResult<SessionDescription, _> = SessionDescription::try_from(sdp.to_string());
    if let Ok(sdp) = sdp {
        for md in &sdp.media_descriptions {
            for attr in &md.attributes {
                if attr.key == "fmtp" {
                    if let Some(value) = &attr.value {
                        return parse_sprop_from_fmtp(value);
                    }
                }
            }
        }
    }
    (None, None)
}

fn parse_sprop_from_fmtp(fmtp: &str) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut sps = None;
    let mut pps = None;
    
    let fmtp = fmtp.split_whitespace().last().unwrap_or(fmtp);
    
    for param in fmtp.split(';') {
        if let Some(params) = param.strip_prefix("sprop-parameter-sets=") {
            for (idx, b64) in params.split(',').enumerate() {
                if b64.is_empty() {
                    continue;
                }
                let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(b64) else {
                    continue;
                };
                if decoded.is_empty() {
                    continue;
                }
                let nal_type = decoded[0] & 0x1F;
                match nal_type {
                    7 if sps.is_none() => sps = Some(decoded),
                    8 if pps.is_none() => pps = Some(decoded),
                    _ => {
                        if idx == 0 && sps.is_none() {
                            sps = Some(decoded);
                        } else if pps.is_none() {
                            pps = Some(decoded);
                        }
                    }
                }
            }
        }
    }
    
    if sps.is_some() || pps.is_some() {
        info!(
            "[WebRTC] SDP sprop-parameter-sets: sps={} pps={}",
            sps.as_ref().map(|s| s.len()).unwrap_or(0),
            pps.as_ref().map(|p| p.len()).unwrap_or(0)
        );
    }
    
    (sps, pps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fmtp_line() {
        let sdp_str = "v=0
o=- 0 0 IN IP4 127.0.0.1
s=-
t=0 0
m=video 9 UDP/TLS/RTP/SAVPF 96
a=rtpmap:96 H264/90000
a=fmtp:96 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f;sprop-parameter-sets=Z0LAHukBQBbsAAADAAQAAAMABAAAAwHNgYI=,aM4xsg==
";
        let sdp: StdResult<SessionDescription, _> = SessionDescription::try_from(sdp_str.to_string());
        println!("SDP parse result: {:?}", sdp);
        if let Ok(sdp) = &sdp {
            println!("Media descriptions: {}", sdp.media_descriptions.len());
            for (i, md) in sdp.media_descriptions.iter().enumerate() {
                println!("MD {} attributes: {}", i, md.attributes.len());
                for attr in &md.attributes {
                    println!("  {}: {:?}", attr.key, attr.value);
                }
            }
        }
        let (sps, pps) = parse_sprop_parameter_sets(sdp_str);
        assert!(sps.is_some());
        assert!(pps.is_some());
        assert_eq!(sps.unwrap()[0] & 0x1F, 7);
        assert_eq!(pps.unwrap()[0] & 0x1F, 8);
    }

    #[test]
    fn builds_and_patches_fmtp() {
        let sps = vec![0x67, 0x42, 0x00, 0x1f, 0x89, 0x8b];
        let pps = vec![0x68, 0x08, 0x07, 0x06];
        let fmtp = build_h264_sdp_fmtp(&sps, &pps);
        assert!(fmtp.contains("profile-level-id=42001f"));
        assert!(fmtp.contains("packetization-mode=1"));
        assert!(fmtp.contains("sprop-parameter-sets="));

        let sdp = "v=0
o=- 0 0 IN IP4 127.0.0.1
s=-
t=0 0
m=video 9 UDP/TLS/RTP/SAVPF 103
a=rtpmap:103 H264/90000
";
        let patched = patch_answer_sdp_h264(sdp, &sps, &pps);
        assert!(patched.contains("a=fmtp:103"));
        assert!(patched.contains("profile-level-id=42001f"));
    }
}