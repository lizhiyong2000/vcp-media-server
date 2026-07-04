use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use tracing::info;

pub fn build_h265_sdp_fmtp(vps: &[u8], sps: &[u8], pps: &[u8]) -> String {
    let vps_b64 = STANDARD.encode(vps);
    let sps_b64 = STANDARD.encode(sps);
    let pps_b64 = STANDARD.encode(pps);
    format!(
        "sprop-vps={vps_b64};sprop-sps={sps_b64};sprop-pps={pps_b64}"
    )
}

/// Find the H265/HEVC dynamic payload type in an SDP.
pub fn h265_payload_type_from_sdp(sdp: &str) -> Option<u8> {
    for line in sdp.lines() {
        let Some(rest) = line.strip_prefix("a=rtpmap:") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let pt: u8 = parts.next()?.parse().ok()?;
        let codec = parts.next()?.to_ascii_lowercase();
        if codec.starts_with("h265/") || codec.starts_with("hevc/") {
            return Some(pt);
        }
    }
    None
}

/// Inject stream VPS/SPS/PPS into the H265 `a=fmtp` line of a play answer SDP.
pub fn patch_answer_sdp_h265(sdp: &str, vps: &[u8], sps: &[u8], pps: &[u8]) -> String {
    let Some(pt) = h265_payload_type_from_sdp(sdp) else {
        return sdp.to_string();
    };
    let fmtp = build_h265_sdp_fmtp(vps, sps, pps);
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
            "[WebRTC] Patched play answer SDP H265 fmtp pt={} vps={}B sps={}B pps={}B",
            pt,
            vps.len(),
            sps.len(),
            pps.len()
        );
    }
    lines.join("\r\n") + "\r\n"
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
        let sdp = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 127\r\na=rtpmap:127 H265/90000\r\n";

        let patched = patch_answer_sdp_h265(sdp, &vps, &sps, &pps);

        assert!(patched.contains("a=fmtp:127"));
        assert!(patched.contains("sprop-vps="));
        assert!(patched.contains("sprop-sps="));
        assert!(patched.contains("sprop-pps="));
    }
}
