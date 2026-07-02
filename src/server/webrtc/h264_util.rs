/// Annex B H264 helpers shared by WebRTC publish/play paths.
use std::time::Duration;

const RTP_CLOCK_HZ: f64 = 90000.0;
const DEFAULT_FRAME_DURATION: Duration = Duration::from_millis(40);

/// Whether `curr` is strictly after `prev` on the 90 kHz RTP clock (handles wrap).
pub fn is_rtp_timestamp_after(curr: u64, prev: u64) -> bool {
    if curr == prev {
        return false;
    }
    (curr as u32).wrapping_sub(prev as u32) < 0x8000_0000
}

/// Whether `curr` is strictly before `prev` on the 90 kHz RTP clock (e.g. encoder reset).
pub fn is_rtp_timestamp_before(curr: u64, prev: u64) -> bool {
    if curr == prev {
        return false;
    }
    !is_rtp_timestamp_after(curr, prev)
}

/// Backward jump smaller than ~1 s at 90 kHz — reorder within the same GOP.
pub fn is_rtp_stale_in_gop(curr: u64, prev: u64) -> bool {
    if !is_rtp_timestamp_before(curr, prev) {
        return false;
    }
    let backward = (prev as u32).wrapping_sub(curr as u32);
    backward > 0 && backward < 90_000
}

/// Backward jump >= ~1 s — encoder reset / new timeline (e.g. replaceTrack).
pub fn is_rtp_timeline_reset(curr: u64, prev: u64) -> bool {
    if !is_rtp_timestamp_before(curr, prev) {
        return false;
    }
    let backward = (prev as u32).wrapping_sub(curr as u32);
    backward >= 90_000
}

/// Sample duration from consecutive RTP timestamps (90 kHz clock).
pub fn duration_from_rtp_timestamps(prev: Option<u64>, curr: u64) -> Duration {
    let Some(prev) = prev else {
        return DEFAULT_FRAME_DURATION;
    };
    let delta = (curr as u32).wrapping_sub(prev as u32);
    if delta == 0 {
        return DEFAULT_FRAME_DURATION;
    }
    let secs = delta as f64 / RTP_CLOCK_HZ;
    let d = Duration::from_secs_f64(secs);
    if d < Duration::from_millis(5) || d > Duration::from_millis(500) {
        DEFAULT_FRAME_DURATION
    } else {
        d
    }
}

pub fn ensure_annex_b(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    if data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1]) {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(4 + data.len());
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(data);
    out
}

/// Split Annex B buffer into NALU payload ranges [start, end).
///
/// Only splits on 4-byte start codes `00 00 00 01`. Using 3-byte `00 00 01`
/// falsely matches inside H264 slice RBSP and breaks SPS/PPS/IDR detection.
pub fn iter_annex_b_nal_ranges(data: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut i = 0;
    while i + 4 <= data.len() {
        if data[i..i + 4] != [0, 0, 0, 1] {
            i += 1;
            continue;
        }
        let start = i + 4;
        let mut end = data.len();
        let mut j = start;
        while j + 4 <= data.len() {
            if data[j..j + 4] == [0, 0, 0, 1] {
                end = j;
                break;
            }
            j += 1;
        }
        if start < end {
            ranges.push((start, end));
        }
        i = if end < data.len() { end } else { data.len() };
    }

    // Single raw NALU without start code (shouldn't happen after depacketize).
    if ranges.is_empty() && !data.is_empty() {
        let start = if data.starts_with(&[0, 0, 0, 1]) {
            4
        } else if data.starts_with(&[0, 0, 1]) {
            3
        } else {
            0
        };
        if start < data.len() {
            ranges.push((start, data.len()));
        }
    }
    ranges
}

pub fn first_nalu_header_offset(data: &[u8]) -> Option<usize> {
    if data.starts_with(&[0, 0, 0, 1]) {
        Some(4)
    } else if data.starts_with(&[0, 0, 1]) {
        Some(3)
    } else if !data.is_empty() {
        Some(0)
    } else {
        None
    }
}

pub fn first_nalu_type(data: &[u8]) -> Option<u8> {
    first_nalu_header_offset(data).and_then(|idx| nalu_type(data, idx))
}

pub fn nalu_type(data: &[u8], header_idx: usize) -> Option<u8> {
    if header_idx < data.len() {
        Some(data[header_idx] & 0x1F)
    } else {
        None
    }
}

pub fn contains_nalu_type(data: &[u8], target: u8) -> bool {
    for (start, end) in iter_annex_b_nal_ranges(data) {
        if let Some(t) = nalu_type(data, start) {
            if t == target {
                return true;
            }
        }
        let _ = end;
    }
    false
}

pub fn contains_idr_nalu(data: &[u8]) -> bool {
    contains_nalu_type(data, 5)
}

/// True if buffer contains a VCL NAL (non-IDR slice or IDR).
pub fn contains_vcl_nalu(data: &[u8]) -> bool {
    for (start, _) in iter_annex_b_nal_ranges(data) {
        if matches!(nalu_type(data, start), Some(1) | Some(5)) {
            return true;
        }
    }
    false
}

pub fn contains_sps_or_pps_nalu(data: &[u8]) -> bool {
    contains_nalu_type(data, 7) || contains_nalu_type(data, 8)
}

/// True when the buffer only carries SPS/PPS/AUD (not video slices).
pub fn is_parameter_set_only(data: &[u8]) -> bool {
    let ranges = iter_annex_b_nal_ranges(data);
    if ranges.is_empty() {
        return false;
    }
    ranges
        .iter()
        .all(|(start, _)| matches!(nalu_type(data, *start), Some(7) | Some(8) | Some(9)))
}

pub fn is_keyframe_annex_b(data: &[u8]) -> bool {
    first_nalu_type(data) == Some(5) || contains_idr_nalu(data)
}

/// Extract raw NALU payloads (without start codes) for SPS/PPS from Annex B.
pub fn extract_sps_pps(data: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut sps = None;
    let mut pps = None;
    for (start, end) in iter_annex_b_nal_ranges(data) {
        match nalu_type(data, start) {
            Some(7) => sps = Some(data[start..end].to_vec()),
            Some(8) => pps = Some(data[start..end].to_vec()),
            _ => {}
        }
    }
    (sps, pps)
}

const NALU_NAMES: [&str; 32] = [
    "unspec", "slice", "dpa", "dpb", "dpc", "idr", "sei", "sps", "pps", "aud", "eoseq", "eostr",
    "fill", "spsext", "prefix", "subset", "depth", "resv17", "resv18", "aux", "ext", "cagg",
    "resv22", "resv23", "unspec24", "unspec25", "unspec26", "unspec27", "unspec28", "unspec29",
    "unspec30", "unspec31",
];

pub fn nalu_type_name(t: u8) -> &'static str {
    let idx = (t & 0x1F) as usize;
    if idx < NALU_NAMES.len() {
        NALU_NAMES[idx]
    } else {
        "unknown"
    }
}

/// Human-readable summary of NALU types in Annex B data (for debug logs).
pub fn describe_annex_b(data: &[u8]) -> String {
    let mut parts = Vec::new();
    for (start, end) in iter_annex_b_nal_ranges(data) {
        if let Some(t) = nalu_type(data, start) {
            let header = data.get(start).copied().unwrap_or(0);
            parts.push(format!(
                "{}(0x{:02x},{}B)",
                nalu_type_name(t),
                header,
                end - start
            ));
        }
    }
    if parts.is_empty() {
        if let Some(t) = first_nalu_type(data) {
            let idx = first_nalu_header_offset(data).unwrap_or(0);
            let header = data.get(idx).copied().unwrap_or(0);
            format!("{}(0x{:02x},{}B)", nalu_type_name(t), header, data.len())
        } else {
            format!("raw:{}B", data.len())
        }
    } else {
        parts.join("+")
    }
}

/// First byte 0x30 with H264 type 16 often means H265 CRA was misread as H264.
pub fn looks_like_h265_misread_as_h264(data: &[u8]) -> bool {
    if let Some(idx) = first_nalu_header_offset(data) {
        if let Some(b) = data.get(idx) {
            // H265 CRA_NUT (type 24) → first header byte 0x30
            return *b == 0x30 || *b == 0x26 || *b == 0x28;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_p_slice_not_false_split() {
        // Simulate ~1KB P-slice: header 0x41 (type=1) + padding that contains 00 00 01 pattern
        let mut nalu = vec![0x00, 0x00, 0x00, 0x01, 0x41];
        nalu.extend(std::iter::repeat(0xFFu8).take(200));
        nalu.extend_from_slice(&[0x00, 0x00, 0x01, 0xAB]); // inside RBSP — must NOT split
        nalu.extend(std::iter::repeat(0xFFu8).take(800));

        let ranges = iter_annex_b_nal_ranges(&nalu);
        assert_eq!(ranges.len(), 1);
        assert_eq!(first_nalu_type(&nalu), Some(1));
    }

    #[test]
    fn stap_a_sps_pps_idr() {
        let mut buf = Vec::new();
        for (header, body) in [
            (0x67u8, vec![0x42u8; 10]),
            (0x68, vec![0xCEu8; 4]),
            (0x65, vec![0x88u8; 20]),
        ] {
            buf.extend_from_slice(&[0, 0, 0, 1]);
            buf.push(header);
            buf.extend(body);
        }
        assert!(contains_sps_or_pps_nalu(&buf));
        assert!(contains_idr_nalu(&buf));
        let (sps, pps) = extract_sps_pps(&buf);
        assert!(sps.is_some());
        assert!(pps.is_some());
    }
}
