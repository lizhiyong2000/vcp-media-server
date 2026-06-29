//! H264 SDP helpers (sprop-parameter-sets, profile-level-id).

use base64::Engine;
use tracing::info;

/// Build WebRTC `a=fmtp` parameter string for H264 from raw SPS/PPS NALUs.
pub fn build_h264_sdp_fmtp(sps: &[u8], pps: &[u8]) -> String {
    let profile = profile_level_id_from_sps(sps).unwrap_or_else(|| "42e01f".to_string());
    let sps_b64 = base64::engine::general_purpose::STANDARD.encode(sps);
    let pps_b64 = base64::engine::general_purpose::STANDARD.encode(pps);
    format!(
        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={profile};sprop-parameter-sets={sps_b64},{pps_b64}"
    )
}

/// Extract `profile-level-id` (3-byte hex) from an SPS NALU.
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

/// Find the H264 dynamic payload type in an SDP.
pub fn h264_payload_type_from_sdp(sdp: &str) -> Option<u8> {
    for line in sdp.lines() {
        let Some(rest) = line.strip_prefix("a=rtpmap:") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let pt: u8 = parts.next()?.parse().ok()?;
        let codec = parts.next()?.to_ascii_lowercase();
        if codec.starts_with("h264/") {
            return Some(pt);
        }
    }
    None
}

/// Inject stream SPS/PPS into the H264 `a=fmtp` line of a play answer SDP.
pub fn patch_answer_sdp_h264(sdp: &str, sps: &[u8], pps: &[u8]) -> String {
    let Some(pt) = h264_payload_type_from_sdp(sdp) else {
        return sdp.to_string();
    };
    let fmtp = build_h264_sdp_fmtp(sps, pps);
    let prefix = format!("a=fmtp:{pt}");
    let new_line = format!("{prefix} {fmtp}");

    let mut lines: Vec<String> = sdp.lines().map(String::from).collect();
    let mut replaced = false;
    for line in &mut lines {
        if line.starts_with(&prefix) {
            *line = new_line.clone();
            replaced = true;
            break;
        }
    }
    if !replaced {
        for i in 0..lines.len() {
            if lines[i].starts_with(&format!("a=rtpmap:{pt}")) {
                lines.insert(i + 1, new_line);
                replaced = true;
                break;
            }
        }
    }
    if replaced {
        info!(
            "[WebRTC] Patched play answer SDP fmtp pt={} profile={}",
            pt,
            profile_level_id_from_sps(sps).unwrap_or_default()
        );
    }
    join_sdp_lines(&lines)
}

fn join_sdp_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\r\n");
    if !out.ends_with("\r\n") {
        out.push_str("\r\n");
    }
    out
}

/// Parse `sprop-parameter-sets` from an SDP offer/answer.
pub fn parse_sprop_parameter_sets(sdp: &str) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut sps = None;
    let mut pps = None;

    for line in sdp.lines() {
        if !line.starts_with("a=fmtp:") {
            continue;
        }
        for part in line.split_whitespace() {
            for param in part.split(';') {
                let Some(params) = param.strip_prefix("sprop-parameter-sets=") else {
                    continue;
                };
                let params = params.trim_end_matches(';');
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
        let sdp = "a=fmtp:96 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f;sprop-parameter-sets=Z0LAHukBQBbsAAADAAQAAAMABAAAAwHNgYI=,aM4xsg==";
        let (sps, pps) = parse_sprop_parameter_sets(sdp);
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

        let sdp = "v=0\r\na=rtpmap:103 H264/90000\r\n";
        let patched = patch_answer_sdp_h264(sdp, &sps, &pps);
        assert!(patched.contains("a=fmtp:103"));
        assert!(patched.contains("profile-level-id=42001f"));
    }
}
