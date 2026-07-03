//! Shared H.264 / H.265 (HEVC) helpers for ingest, egress, and parameter-set handling.

use base64::Engine;
use bytes::Bytes;

use super::CodecType;

pub const ANNEX_B_PREFIX: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

// H.264 NAL types
pub const H264_NAL_SPS: u8 = 7;
pub const H264_NAL_PPS: u8 = 8;
pub const H264_NAL_IDR: u8 = 5;

// H.265 NAL types (nuh_type)
pub const H265_NAL_VPS: u8 = 32;
pub const H265_NAL_SPS: u8 = 33;
pub const H265_NAL_PPS: u8 = 34;
pub const H265_NAL_IDR_W_RADL: u8 = 19;
pub const H265_NAL_IDR_N_LP: u8 = 20;
pub const H265_NAL_CRA: u8 = 21;
pub const H265_NAL_AUD: u8 = 35;

pub fn h264_nalu_type(byte: u8) -> u8 {
    byte & 0x1F
}

pub fn hevc_nalu_type(byte: u8) -> u8 {
    (byte >> 1) & 0x3F
}

pub fn iter_annex_b_nal_ranges(data: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut i = 0;
    while i + 4 <= data.len() {
        if data[i..i + 4] != ANNEX_B_PREFIX {
            i += 1;
            continue;
        }
        let start = i + 4;
        let mut end = data.len();
        let mut j = start;
        while j + 4 <= data.len() {
            if data[j..j + 4] == ANNEX_B_PREFIX {
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
    if ranges.is_empty() && !data.is_empty() {
        let start = if data.starts_with(&ANNEX_B_PREFIX) {
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

pub fn contains_h264_idr(data: &[u8]) -> bool {
    for (start, _) in iter_annex_b_nal_ranges(data) {
        if h264_nalu_type(data[start]) == H264_NAL_IDR {
            return true;
        }
    }
    false
}

pub fn contains_hevc_idr(data: &[u8]) -> bool {
    for (start, _) in iter_annex_b_nal_ranges(data) {
        match hevc_nalu_type(data[start]) {
            H265_NAL_IDR_W_RADL | H265_NAL_IDR_N_LP | H265_NAL_CRA => return true,
            _ => {}
        }
    }
    false
}

pub fn is_keyframe_annex_b(codec: CodecType, data: &[u8]) -> bool {
    match codec {
        CodecType::H265 => contains_hevc_idr(data),
        CodecType::H264 | _ => contains_h264_idr(data),
    }
}

pub fn is_parameter_set_only(codec: CodecType, data: &[u8]) -> bool {
    let ranges = iter_annex_b_nal_ranges(data);
    if ranges.is_empty() {
        return false;
    }
    ranges.iter().all(|(start, _)| match codec {
        CodecType::H265 => matches!(
            hevc_nalu_type(data[*start]),
            H265_NAL_VPS | H265_NAL_SPS | H265_NAL_PPS | H265_NAL_AUD
        ),
        _ => matches!(
            h264_nalu_type(data[*start]),
            H264_NAL_SPS | H264_NAL_PPS | 9
        ),
    })
}

pub fn extract_h264_sps_pps(data: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut sps = None;
    let mut pps = None;
    for (start, end) in iter_annex_b_nal_ranges(data) {
        match h264_nalu_type(data[start]) {
            H264_NAL_SPS if sps.is_none() => sps = Some(data[start..end].to_vec()),
            H264_NAL_PPS if pps.is_none() => pps = Some(data[start..end].to_vec()),
            _ => {}
        }
    }
    (sps, pps)
}

pub fn extract_hevc_param_sets(data: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut vps = None;
    let mut sps = None;
    let mut pps = None;
    for (start, end) in iter_annex_b_nal_ranges(data) {
        match hevc_nalu_type(data[start]) {
            H265_NAL_VPS if vps.is_none() => vps = Some(data[start..end].to_vec()),
            H265_NAL_SPS if sps.is_none() => sps = Some(data[start..end].to_vec()),
            H265_NAL_PPS if pps.is_none() => pps = Some(data[start..end].to_vec()),
            _ => {}
        }
    }
    (vps, sps, pps)
}

pub fn merge_annex_b_nalu_config(
    codec: CodecType,
    nalu: &[u8],
) -> Option<(Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>)> {
    if nalu.is_empty() {
        return None;
    }
    match codec {
        CodecType::H265 => {
            let t = hevc_nalu_type(nalu[0]);
            Some(match t {
                H265_NAL_VPS => (Some(nalu.to_vec()), None, None),
                H265_NAL_SPS => (None, Some(nalu.to_vec()), None),
                H265_NAL_PPS => (None, None, Some(nalu.to_vec())),
                _ => return None,
            })
        }
        _ => {
            let t = h264_nalu_type(nalu[0]);
            Some(match t {
                H264_NAL_SPS => (None, Some(nalu.to_vec()), None),
                H264_NAL_PPS => (None, None, Some(nalu.to_vec())),
                _ => return None,
            })
        }
    }
}

pub fn prepend_annex_b_config(
    codec: CodecType,
    vps: Option<&[u8]>,
    sps: Option<&[u8]>,
    pps: Option<&[u8]>,
    access_unit: &[u8],
) -> Vec<u8> {
    if is_parameter_set_only(codec, access_unit) {
        return access_unit.to_vec();
    }
    let mut out = Vec::new();
    if codec == CodecType::H265 {
        if let Some(v) = vps {
            out.extend_from_slice(&ANNEX_B_PREFIX);
            out.extend_from_slice(v);
        }
    }
    if let Some(s) = sps {
        out.extend_from_slice(&ANNEX_B_PREFIX);
        out.extend_from_slice(s);
    }
    if let Some(p) = pps {
        out.extend_from_slice(&ANNEX_B_PREFIX);
        out.extend_from_slice(p);
    }
    out.extend_from_slice(access_unit);
    out
}

/// Parse RTMP HEVCDecoderConfigurationRecord (after the 5-byte video tag header).
pub fn parse_hevc_decoder_config_record(
    config: &[u8],
) -> (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>) {
    if config.len() < 23 {
        return (None, None, None);
    }
    let num_arrays = config[22] as usize;
    let mut offset = 23usize;
    let mut vps = None;
    let mut sps = None;
    let mut pps = None;
    for _ in 0..num_arrays {
        if offset + 3 > config.len() {
            break;
        }
        let nal_type = config[offset] & 0x3F;
        offset += 1;
        let num_nalus = u16::from_be_bytes([config[offset], config[offset + 1]]) as usize;
        offset += 2;
        for _ in 0..num_nalus {
            if offset + 2 > config.len() {
                break;
            }
            let len = u16::from_be_bytes([config[offset], config[offset + 1]]) as usize;
            offset += 2;
            if offset + len > config.len() {
                break;
            }
            let nal = config[offset..offset + len].to_vec();
            match nal_type {
                H265_NAL_VPS if vps.is_none() => vps = Some(nal),
                H265_NAL_SPS if sps.is_none() => sps = Some(nal),
                H265_NAL_PPS if pps.is_none() => pps = Some(nal),
                _ => {}
            }
            offset += len;
        }
    }
    (vps, sps, pps)
}

pub fn build_hevc_decoder_config_record(vps: &[u8], sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x01); // configurationVersion
    out.push(0x01); // general_profile_space/tier/profile_idc placeholder
    out.extend_from_slice(&[0x60, 0x00, 0x00, 0x00]); // general_profile_compatibility_flags
    out.extend_from_slice(&[0x90, 0x00, 0x00, 0x00, 0x00, 0x00]); // constraint_indicator_flags
    out.push(0x5D); // general_level_idc
    out.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc
    out.push(0xFC); // parallelismType
    out.push(0xFD); // chromaFormat
    out.push(0xF8); // bitDepthLumaMinus8
    out.push(0xF8); // bitDepthChromaMinus8
    out.extend_from_slice(&[0x00, 0x00]); // avgFrameRate
    out.push(0x0F); // constantFrameRate/numTemporalLayers/temporalIdNested/lengthSizeMinusOne
    out.push(0x03); // numOfArrays (VPS/SPS/PPS)
    for (nal_type, nal) in [
        (H265_NAL_VPS, vps),
        (H265_NAL_SPS, sps),
        (H265_NAL_PPS, pps),
    ] {
        out.push(0x80 | nal_type);
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

/// Enhanced RTMP (E-RTMP) video tag constants — see Veovera enhanced-rtmp spec.
pub const FLV_IS_EX_HEADER: u8 = 0x80;
pub const FLV_FRAME_KEY: u8 = 0x10;
pub const FLV_FRAME_INTER: u8 = 0x20;
pub const RTMP_PKT_SEQUENCE_START: u8 = 0;
pub const RTMP_PKT_CODED_FRAMES: u8 = 1;
pub const RTMP_PKT_CODED_FRAMES_X: u8 = 3;
pub const HEVC_FOURCC: [u8; 4] = *b"hvc1";

pub fn is_enhanced_rtmp_video(data: &[u8]) -> bool {
    !data.is_empty() && (data[0] & FLV_IS_EX_HEADER) != 0
}

pub fn enhanced_rtmp_is_keyframe(header: u8) -> bool {
    (header & 0x70) == FLV_FRAME_KEY
}

pub fn enhanced_rtmp_packet_type(header: u8) -> u8 {
    header & 0x0F
}

pub struct EnhancedHevcVideoMessage<'a> {
    pub packet_type: u8,
    pub is_keyframe: bool,
    pub payload: &'a [u8],
}

pub fn parse_enhanced_hevc_video_message(data: &[u8]) -> Option<EnhancedHevcVideoMessage<'_>> {
    if data.len() < 6 || !is_enhanced_rtmp_video(data) || &data[1..5] != HEVC_FOURCC {
        return None;
    }
    let packet_type = enhanced_rtmp_packet_type(data[0]);
    let payload = match packet_type {
        RTMP_PKT_SEQUENCE_START | RTMP_PKT_CODED_FRAMES_X => &data[5..],
        RTMP_PKT_CODED_FRAMES if data.len() >= 9 => &data[8..],
        _ => return None,
    };
    Some(EnhancedHevcVideoMessage {
        packet_type,
        is_keyframe: enhanced_rtmp_is_keyframe(data[0]),
        payload,
    })
}

pub fn hvcc_payload_to_annex_b(payload: &[u8]) -> Vec<u8> {
    let mut annex_b = Vec::with_capacity(payload.len());
    let mut offset = 0;
    while offset + 4 <= payload.len() {
        let nalu_len = u32::from_be_bytes([
            payload[offset],
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
        ]) as usize;
        offset += 4;
        if offset + nalu_len > payload.len() {
            break;
        }
        annex_b.extend_from_slice(&ANNEX_B_PREFIX);
        annex_b.extend_from_slice(&payload[offset..offset + nalu_len]);
        offset += nalu_len;
    }
    annex_b
}

pub fn build_rtmp_hevc_sequence_header(vps: &[u8], sps: &[u8], pps: &[u8]) -> Vec<u8> {
    build_enhanced_rtmp_hevc_sequence_header(&build_hevc_decoder_config_record(vps, sps, pps))
}

pub fn build_enhanced_rtmp_hevc_sequence_header(hvcc: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(FLV_IS_EX_HEADER | FLV_FRAME_KEY | RTMP_PKT_SEQUENCE_START);
    data.extend_from_slice(&HEVC_FOURCC);
    data.extend_from_slice(hvcc);
    data
}

pub fn filter_hevc_vcl_annex_b(annex_b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (start, end) in iter_annex_b_nal_ranges(annex_b) {
        let nal = &annex_b[start..end];
        if nal.is_empty() {
            continue;
        }
        let t = hevc_nalu_type(nal[0]);
        if matches!(t, H265_NAL_VPS | H265_NAL_SPS | H265_NAL_PPS | H265_NAL_AUD) {
            continue;
        }
        out.extend_from_slice(&ANNEX_B_PREFIX);
        out.extend_from_slice(nal);
    }
    out
}

pub fn annex_b_to_hvcc_payload(annex_b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (start, end) in iter_annex_b_nal_ranges(annex_b) {
        let nal = &annex_b[start..end];
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

pub fn build_rtmp_hevc_video_nalu(frame_type_nibble: u8, annex_b: &[u8]) -> Vec<u8> {
    let frame_type = if frame_type_nibble == FLV_FRAME_KEY {
        FLV_FRAME_KEY
    } else {
        FLV_FRAME_INTER
    };
    let mut data = Vec::new();
    // PacketTypeCodedFrames + composition time 0 matches ffmpeg Enhanced RTMP output (0x91/0xA1).
    data.push(FLV_IS_EX_HEADER | frame_type | RTMP_PKT_CODED_FRAMES);
    data.extend_from_slice(&HEVC_FOURCC);
    data.extend_from_slice(&[0x00, 0x00, 0x00]);
    data.extend(annex_b_to_hvcc_payload(annex_b));
    data
}

pub fn build_rtmp_h264_video_nalu(frame_type_nibble: u8, annex_b: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(frame_type_nibble | 0x07);
    data.push(0x01);
    data.extend_from_slice(&[0x00, 0x00, 0x00]);
    data.extend(annex_b_to_hvcc_payload(annex_b));
    data
}

pub fn ensure_annex_b(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    if data.starts_with(&ANNEX_B_PREFIX) || data.starts_with(&[0, 0, 1]) {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(4 + data.len());
    out.extend_from_slice(&ANNEX_B_PREFIX);
    out.extend_from_slice(data);
    out
}

pub fn build_rtmp_video_from_annex_b(frame: &super::MediaFrame) -> Vec<u8> {
    let annex_b = ensure_annex_b(&frame.data);
    let frame_type = if frame.codec == CodecType::H265 {
        if is_keyframe_annex_b(frame.codec, &annex_b) {
            0x10
        } else {
            0x20
        }
    } else if frame.is_keyframe || is_keyframe_annex_b(frame.codec, &annex_b) {
        0x10
    } else {
        0x20
    };
    match frame.codec {
        CodecType::H265 => {
            let vcl = filter_hevc_vcl_annex_b(&annex_b);
            if vcl.is_empty() {
                Vec::new()
            } else {
                build_rtmp_hevc_video_nalu(frame_type, &vcl)
            }
        }
        _ => build_rtmp_h264_video_nalu(frame_type, &annex_b),
    }
}

pub fn hevc_sprop_from_nals(vps: &[u8], sps: &[u8], pps: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    format!(
        "sprop-vps={};sprop-sps={};sprop-pps={}",
        b64.encode(vps),
        b64.encode(sps),
        b64.encode(pps)
    )
}

pub fn parse_hevc_sprop_fmtp(line: &str) -> (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>) {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut vps = None;
    let mut sps = None;
    let mut pps = None;
    for part in line.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("sprop-vps=") {
            if let Ok(decoded) = b64.decode(v.trim()) {
                vps = Some(decoded);
            }
        } else if let Some(v) = part.strip_prefix("sprop-sps=") {
            if let Ok(decoded) = b64.decode(v.trim()) {
                sps = Some(decoded);
            }
        } else if let Some(v) = part.strip_prefix("sprop-pps=") {
            if let Ok(decoded) = b64.decode(v.trim()) {
                pps = Some(decoded);
            }
        }
    }
    (vps, sps, pps)
}

pub fn prepare_video_au_for_ts(codec: CodecType, data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    if starts_with_aud(codec, data) {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(7 + data.len());
    out.extend_from_slice(&ANNEX_B_PREFIX);
    match codec {
        CodecType::H265 => out.extend_from_slice(&[0x46, 0x01, 0x50]), // HEVC AUD NUT
        _ => out.extend_from_slice(&[0x09, 0xF0]),                     // H264 AUD
    }
    out.extend_from_slice(data);
    out
}

fn starts_with_aud(codec: CodecType, data: &[u8]) -> bool {
    for (start, _) in iter_annex_b_nal_ranges(data) {
        return match codec {
            CodecType::H265 => hevc_nalu_type(data[start]) == H265_NAL_AUD,
            _ => h264_nalu_type(data[start]) == 9,
        };
    }
    false
}

pub const UDP_RTP_MAX_PAYLOAD: usize = 1200;

pub fn packetize_hevc_nal_for_rtp(
    nal: &[u8],
    payload_type: u8,
    seq: &mut u16,
    ts: u32,
    ssrc: u32,
    marker: bool,
) -> Vec<Vec<u8>> {
    if nal.len() < 2 {
        return Vec::new();
    }
    if nal.len() <= UDP_RTP_MAX_PAYLOAD {
        let pkt = build_rtp_packet(payload_type, *seq, ts, ssrc, marker, nal);
        *seq = seq.wrapping_add(1);
        return vec![pkt];
    }
    let nal_type = hevc_nalu_type(nal[0]);
    let layer_id = ((nal[0] & 0x01) << 5) | (nal[1] >> 3);
    let tid = nal[1] & 0x07;
    let fu_type = 49u8;
    let fu_header0 = (fu_type << 1) | ((layer_id >> 5) & 0x01);
    let fu_header1 = ((layer_id & 0x1F) << 3) | tid;
    let payload = &nal[2..];
    let chunk_max = UDP_RTP_MAX_PAYLOAD - 3;
    let mut packets = Vec::new();
    let mut offset = 0;
    while offset < payload.len() {
        let chunk_size = (payload.len() - offset).min(chunk_max);
        let is_start = offset == 0;
        let is_end = offset + chunk_size >= payload.len();
        let fu_hdr =
            (if is_start { 0x80 } else { 0 }) | (if is_end { 0x40 } else { 0 }) | (nal_type & 0x3F);
        let mut rtp_payload = vec![fu_header0, fu_header1, fu_hdr];
        rtp_payload.extend_from_slice(&payload[offset..offset + chunk_size]);
        let mark = marker && is_end;
        packets.push(build_rtp_packet(
            payload_type,
            *seq,
            ts,
            ssrc,
            mark,
            &rtp_payload,
        ));
        *seq = seq.wrapping_add(1);
        offset += chunk_size;
    }
    packets
}

pub fn packetize_hevc_access_unit_for_rtp(
    annex_b: &[u8],
    payload_type: u8,
    seq: &mut u16,
    ts: u32,
    ssrc: u32,
) -> Vec<Vec<u8>> {
    let ranges = iter_annex_b_nal_ranges(annex_b);
    let mut packets = Vec::new();
    for (i, (start, end)) in ranges.iter().enumerate() {
        let marker = i + 1 == ranges.len();
        packets.extend(packetize_hevc_nal_for_rtp(
            &annex_b[*start..*end],
            payload_type,
            seq,
            ts,
            ssrc,
            marker,
        ));
    }
    packets
}

pub fn build_rtp_packet(
    payload_type: u8,
    seq: u16,
    ts: u32,
    ssrc: u32,
    marker: bool,
    payload: &[u8],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(12 + payload.len());
    buf.push((2 << 6) | 0);
    buf.push(((marker as u8) << 7) | payload_type);
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(&ts.to_be_bytes());
    buf.extend_from_slice(&ssrc.to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Reassemble HEVC FU (type 49) RTP payloads into Annex-B access units.
pub struct HevcFuReassembler {
    buffer: Vec<u8>,
    header: [u8; 2],
}

impl Default for HevcFuReassembler {
    fn default() -> Self {
        Self {
            buffer: Vec::new(),
            header: [0; 2],
        }
    }
}

impl HevcFuReassembler {
    pub fn depacketize(&mut self, payload: &[u8]) -> Option<(Bytes, bool)> {
        if payload.len() < 3 {
            return None;
        }
        let nal_type = hevc_nalu_type(payload[0]);
        if nal_type == 49 {
            let fu_header = payload[2];
            let start = (fu_header & 0x80) != 0;
            let end = (fu_header & 0x40) != 0;
            let orig_type = fu_header & 0x3F;
            if start {
                self.buffer.clear();
                self.header = [(orig_type << 1) | (payload[0] & 0x01), payload[1]];
                self.buffer.extend_from_slice(&payload[3..]);
            } else if !self.buffer.is_empty() {
                self.buffer.extend_from_slice(&payload[3..]);
            } else {
                return None;
            }
            if end {
                let mut annex_b = Vec::with_capacity(4 + 2 + self.buffer.len());
                annex_b.extend_from_slice(&ANNEX_B_PREFIX);
                annex_b.push(self.header[0]);
                annex_b.push(self.header[1]);
                annex_b.extend_from_slice(&self.buffer);
                let is_keyframe = matches!(orig_type, H265_NAL_IDR_W_RADL | H265_NAL_IDR_N_LP);
                self.buffer.clear();
                return Some((Bytes::from(annex_b), is_keyframe));
            }
            return None;
        }
        if payload.len() >= 2 {
            let mut annex_b = Vec::with_capacity(4 + payload.len());
            annex_b.extend_from_slice(&ANNEX_B_PREFIX);
            annex_b.extend_from_slice(payload);
            let is_keyframe = matches!(
                hevc_nalu_type(payload[0]),
                H265_NAL_IDR_W_RADL | H265_NAL_IDR_N_LP
            );
            return Some((Bytes::from(annex_b), is_keyframe));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hevc_idr_detection() {
        let idr = [0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xAF];
        assert!(contains_hevc_idr(&idr));
    }

    #[test]
    fn hevc_cra_is_treated_as_random_access_keyframe() {
        let cra = [0x00, 0x00, 0x00, 0x01, 0x2a, 0x01, 0xaf];
        assert!(contains_hevc_idr(&cra));
    }

    #[test]
    fn h264_idr_detection() {
        let idr = [0x00, 0x00, 0x00, 0x01, 0x65, 0x88];
        assert!(contains_h264_idr(&idr));
    }

    #[test]
    fn enhanced_rtmp_hevc_sequence_header_format() {
        let vps = vec![0x40, 0x01, 0x0c, 0x01];
        let sps = vec![0x42, 0x01, 0x01, 0x60];
        let pps = vec![0x44, 0x01, 0xc0, 0x73];
        let header = build_rtmp_hevc_sequence_header(&vps, &sps, &pps);
        assert_eq!(
            header[0],
            FLV_IS_EX_HEADER | FLV_FRAME_KEY | RTMP_PKT_SEQUENCE_START
        );
        assert_eq!(&header[1..5], b"hvc1");
        let parsed = parse_enhanced_hevc_video_message(&header).expect("enhanced header");
        assert_eq!(parsed.packet_type, RTMP_PKT_SEQUENCE_START);
        assert!(!parsed.payload.is_empty());
        let (parsed_vps, parsed_sps, parsed_pps) = parse_hevc_decoder_config_record(parsed.payload);
        assert_eq!(parsed_vps.as_deref(), Some(vps.as_slice()));
        assert_eq!(parsed_sps.as_deref(), Some(sps.as_slice()));
        assert_eq!(parsed_pps.as_deref(), Some(pps.as_slice()));
    }

    #[test]
    fn built_hevc_decoder_config_record_round_trips_parameter_sets() {
        let vps = vec![0x40, 0x01, 0x0c, 0x01];
        let sps = vec![0x42, 0x01, 0x01, 0x60];
        let pps = vec![0x44, 0x01, 0xc0, 0x73];
        let config = build_hevc_decoder_config_record(&vps, &sps, &pps);

        let (parsed_vps, parsed_sps, parsed_pps) = parse_hevc_decoder_config_record(&config);

        assert_eq!(parsed_vps.as_deref(), Some(vps.as_slice()));
        assert_eq!(parsed_sps.as_deref(), Some(sps.as_slice()));
        assert_eq!(parsed_pps.as_deref(), Some(pps.as_slice()));
    }

    #[test]
    fn prepare_h265_au_for_ts_prepends_valid_hevc_aud() {
        let idr = [0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xaf];

        let au = prepare_video_au_for_ts(CodecType::H265, &idr);

        assert_eq!(&au[..7], &[0x00, 0x00, 0x00, 0x01, 0x46, 0x01, 0x50]);
        assert_eq!(hevc_nalu_type(au[4]), H265_NAL_AUD);
        assert_eq!(&au[7..], &idr);
    }

    #[test]
    fn prepare_h265_au_for_ts_does_not_duplicate_existing_aud() {
        let au = [
            0x00, 0x00, 0x00, 0x01, 0x46, 0x01, 0x50, 0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xaf,
        ];

        assert_eq!(prepare_video_au_for_ts(CodecType::H265, &au), au);
    }
}
