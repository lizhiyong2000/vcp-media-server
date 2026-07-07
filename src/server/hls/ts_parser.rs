/// Minimal MPEG-TS parser for HLS puller.
/// Extracts H264/H265 video and AAC audio from TS segments.
use bytes::Bytes;
use std::collections::HashMap;
use tracing::debug;

const TS_PACKET_SIZE: usize = 188;
const TS_SYNC_BYTE: u8 = 0x47;

/// PID for PAT (Program Association Table)
const PAT_PID: u16 = 0x0000;

/// Extracted ES data with codec info
#[derive(Debug, Clone)]
pub struct TsElementaryData {
    pub codec: TsCodec,
    /// Accumulated PES payload (Annex B for video, raw AAC for audio)
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TsCodec {
    #[default]
    Unknown,
    H264,
    H265,
    AAC,
}

/// Find sync byte 0x47 at TS_PACKET_SIZE intervals. Returns list of complete 188-byte packets.
pub fn find_ts_packets(data: &[u8]) -> Vec<&[u8]> {
    let mut packets = Vec::new();
    let mut offset = 0;

    // Find the first sync byte
    while offset < data.len() {
        if offset + TS_PACKET_SIZE > data.len() {
            break;
        }
        if data[offset] == TS_SYNC_BYTE {
            // Verify it's a proper sync by checking the next packet too
            if offset + TS_PACKET_SIZE + 1 <= data.len() {
                let next_expected = offset + TS_PACKET_SIZE;
                if next_expected + TS_PACKET_SIZE <= data.len() {
                    if data[next_expected] == TS_SYNC_BYTE {
                        // Found valid sync - collect all packets from here
                        let mut pos = offset;
                        while pos + TS_PACKET_SIZE <= data.len() && data[pos] == TS_SYNC_BYTE {
                            packets.push(&data[pos..pos + TS_PACKET_SIZE]);
                            pos += TS_PACKET_SIZE;
                        }
                        return packets;
                    }
                } else {
                    // Only one packet worth of data left, accept it
                    packets.push(&data[offset..offset + TS_PACKET_SIZE]);
                    return packets;
                }
            } else {
                // Last partial chunk
                if data[offset] == TS_SYNC_BYTE && offset + TS_PACKET_SIZE <= data.len() {
                    packets.push(&data[offset..offset + TS_PACKET_SIZE]);
                }
                return packets;
            }
        }
        offset += 1;
    }
    packets
}

/// Parse a Program Association Table to find PMT PIDs
pub fn parse_pat(packets: &[&[u8]]) -> Vec<u16> {
    let mut pmt_pids = Vec::new();
    for pkt in packets {
        let pid = ts_pid(pkt);
        if pid != PAT_PID {
            continue;
        }
        // Only process packets with Payload Unit Start Indicator set
        let pusi = (pkt[1] & 0x40) != 0;
        if !pusi {
            continue;
        }
        let offset = ts_payload_offset(pkt);
        if offset >= TS_PACKET_SIZE {
            continue;
        }
        // Skip pointer_field (1 byte) to reach section start
        let section_start = offset + 1 + pkt[offset] as usize;
        if section_start >= TS_PACKET_SIZE {
            continue;
        }
        let payload = &pkt[section_start..];

        if payload.len() < 3 {
            continue;
        }
        // PAT: table_id(1) + section_length(2) + ... 
        // section_syntax_indicator at bit 7 of byte 1
        let section_length = ((payload[1] as usize & 0x0F) << 8) | payload[2] as usize;
        let section_end = std::cmp::min(3 + section_length, payload.len());
        let table_data = &payload[3..section_end];
        if table_data.len() >= 9 {
            // PAT section body:
            //   transport_stream_id:               2 bytes (table_data[0..2])
            //   reserved+version+current_next:      1 byte  (table_data[2])
            //   section_number:                     1 byte  (table_data[3])
            //   last_section_number:                1 byte  (table_data[4])
            //   programs:                           variable (each 4 bytes)
            //   CRC_32:                             last 4 bytes
            // Skip 5-byte header and exclude 4-byte CRC
            let body_end = table_data.len().saturating_sub(4);
            let programs = &table_data[5..body_end];
            for chunk in programs.chunks(4) {
                if chunk.len() < 4 {
                    break;
                }
                let program_number = ((chunk[0] as u16) << 8) | chunk[1] as u16;
                let program_map_pid = ((chunk[2] as u16 & 0x1F) << 8) | chunk[3] as u16;
                if program_number != 0 {
                    pmt_pids.push(program_map_pid);
                }
            }
        }
    }
    pmt_pids
}

/// Parse a Program Map Table to find ES PIDs for video and audio
#[derive(Debug, Clone, Copy, Default)]
pub struct PmtInfo {
    pub video_pid: Option<u16>,
    pub audio_pid: Option<u16>,
    pub video_codec: TsCodec,
}

pub fn parse_pmt(packets: &[&[u8]], pmt_pid: u16) -> (Option<u16>, Option<u16>) {
    let info = parse_pmt_info(packets, pmt_pid);
    (info.video_pid, info.audio_pid)
}

pub fn parse_pmt_info(packets: &[&[u8]], pmt_pid: u16) -> PmtInfo {
    let mut video_pid = None;
    let mut audio_pid = None;
    let mut video_codec = TsCodec::Unknown;

    for pkt in packets {
        if ts_pid(pkt) != pmt_pid {
            continue;
        }
        // Only process packets with Payload Unit Start Indicator set
        let pusi = (pkt[1] & 0x40) != 0;
        if !pusi {
            continue;
        }
        let offset = ts_payload_offset(pkt);
        if offset >= TS_PACKET_SIZE {
            continue;
        }
        // Skip pointer_field to reach section start
        let section_start = offset + 1 + pkt[offset] as usize;
        if section_start >= TS_PACKET_SIZE {
            continue;
        }
        let payload = &pkt[section_start..];
        if payload.len() < 4 {
            continue;
        }

        let section_length = ((payload[1] as usize & 0x0F) << 8) | payload[2] as usize;
        let section_end = std::cmp::min(3 + section_length, payload.len());

        // Fixed PMT header: table_id(1) + section_length(2) + program_number(2) + version(1) +
        // section_number(1) + last_section_number(1) + PCR_PID(2) + program_info_length(2) = 12 bytes
        if section_end < 12 {
            continue;
        }
        let program_info_len =
            ((payload[10] as usize & 0x0F) << 8) | payload[11] as usize;
        let mut offset = 12 + program_info_len;

        // Exclude CRC_32 (last 4 bytes) from stream entry parsing
        let pmt_data_end = section_end.saturating_sub(4);
        while offset + 5 <= pmt_data_end {
            let stream_type = payload[offset];
            let elementary_pid = ((payload[offset + 1] as u16 & 0x1F) << 8) | payload[offset + 2] as u16;
            let es_info_len =
                ((payload[offset + 3] as usize & 0x0F) << 8) | payload[offset + 4] as usize;

            match stream_type {
                0x1B => {
                    // H264 AVC
                    debug!("[TS] Found H264 video PID={}", elementary_pid);
                    video_pid = Some(elementary_pid);
                    video_codec = TsCodec::H264;
                }
                0x24 => {
                    // H265 HEVC
                    debug!("[TS] Found H265 video PID={}", elementary_pid);
                    video_pid = Some(elementary_pid);
                    video_codec = TsCodec::H265;
                }
                0x0F | 0x11 => {
                    // AAC / AAC LATM
                    debug!("[TS] Found AAC audio PID={}", elementary_pid);
                    if audio_pid.is_none() {
                        audio_pid = Some(elementary_pid);
                    }
                }
                _ => {
                    // Other stream types
                }
            }
            offset += 5 + es_info_len;
        }
    }
    PmtInfo {
        video_pid,
        audio_pid,
        video_codec,
    }
}

/// Extract ES data from TS packets, grouped by PID.
/// Returns map from PID to accumulated PES data (with PES headers stripped, and codec identified).
pub fn extract_es_data(
    packets: &[&[u8]],
    video_pid: u16,
    audio_pid: u16,
) -> HashMap<u16, TsElementaryData> {
    let mut es_map: HashMap<u16, TsElementaryData> = HashMap::new();
    // PES reassembly buffers per PID
    let mut pes_buf: HashMap<u16, Vec<u8>> = HashMap::new();

    for pkt in packets {
        let pid = ts_pid(pkt);
        if pid != video_pid && pid != audio_pid {
            continue;
        }
        let pusi = (pkt[1] & 0x40) != 0; // payload_unit_start_indicator
        let has_adaptation = (pkt[3] & 0x20) != 0;
        let has_payload = (pkt[3] & 0x10) != 0;
        if !has_payload {
            continue;
        }

        let mut payload_offset = 4; // after 4-byte header
        if has_adaptation {
            let adaptation_len = pkt[payload_offset] as usize;
            payload_offset += 1 + adaptation_len;
        }
        if payload_offset >= TS_PACKET_SIZE {
            continue;
        }
        let raw_payload = &pkt[payload_offset..];

        if pusi {
            // Start of new PES packet - flush previous
            if let Some(buf) = pes_buf.remove(&pid) {
                process_pes_packet(&buf, pid, video_pid, &mut es_map);
            }
        }

        pes_buf
            .entry(pid)
            .or_default()
            .extend_from_slice(raw_payload);
    }

    // Flush remaining
    for (pid, buf) in pes_buf.drain() {
        process_pes_packet(&buf, pid, video_pid, &mut es_map);
    }

    es_map
}

fn process_pes_packet(
    buf: &[u8],
    pid: u16,
    video_pid: u16,
    es_map: &mut HashMap<u16, TsElementaryData>,
) {
    if buf.len() < 6 {
        return;
    }

    // PES header structure:
    // packet_start_code_prefix: 3 bytes (0x00 0x00 0x01)
    // stream_id: 1 byte
    // PES_packet_length: 2 bytes
    // Then optional PES header...

    // Quick check: do we have a PES start code?
    // Sometimes TS payload doesn't start with PES header for continuation packets
    // Only process if there's enough data
    if buf.len() < 9 {
        return;
    }

    // Look for PES header within first 3 bytes - for actual PES packets
    let mut pes_data = buf;

    // Strip TS adaptation stuffing if present
    // For PUSI=true packets, PES header starts immediately
    match pes_data.windows(4).position(|w| w == [0x00, 0x00, 0x01, b'\xE0']
        || w == [0x00, 0x00, 0x01, b'\xC0'])
    {
        Some(offset) if offset < 3 => {
            if offset > 0 {
                pes_data = &pes_data[offset..];
            }
        }
        // No PES header found, treat as continuation data
        _ => {
            let codec = pid_to_codec(pid, video_pid);
            let entry = es_map.entry(pid).or_insert(TsElementaryData {
                codec,
                data: Vec::new(),
            });
            entry.data.extend_from_slice(buf);
            return;
        }
    }

    if pes_data.len() < 9 {
        return;
    }

    // stream_id at byte 3
    let _stream_id = pes_data[3];
    let _pes_length = ((pes_data[4] as u16) << 8) | pes_data[5] as u16;

    // PES header data length at offset 8 (after marker bits etc)
    let pes_header_data_len = pes_data[8] as usize;

    // Data starts after: start_code(3) + stream_id(1) + PES_length(2) + flags(2) + header_len(1) + header_data(N)
    let data_offset = 9 + pes_header_data_len;
    if data_offset >= pes_data.len() {
        return;
    }

    let codec = pid_to_codec(pid, video_pid);
    let entry = es_map.entry(pid).or_insert(TsElementaryData {
        codec,
        data: Vec::new(),
    });
    entry.data.extend_from_slice(&pes_data[data_offset..]);
}

fn pid_to_codec(pid: u16, video_pid: u16) -> TsCodec {
    if pid == video_pid {
        // We don't know H264 vs H265 from PID alone - will be determined later
        // Checking PMT stream_type requires context, so default to H264 here
        // and detect from NAL type during frame extraction
        TsCodec::H264
    } else {
        TsCodec::AAC
    }
}

fn ts_pid(pkt: &[u8]) -> u16 {
    ((pkt[1] as u16 & 0x1F) << 8) | pkt[2] as u16
}

fn ts_payload_offset(pkt: &[u8]) -> usize {
    let has_adaptation = (pkt[3] & 0x20) != 0;
    let mut offset = 4;
    if has_adaptation {
        let adaptation_len = pkt[offset] as usize;
        offset += 1 + adaptation_len;
    }
    offset
}

/// Extract H264/H265 frames from Annex B format ES data, returning individual NAL units.
/// Returns (codec, is_keyframe, nal_unit_data). Only returns video slice NALs + SPS/PPS/VPS.
pub fn extract_video_frames(data: &[u8]) -> Vec<(TsCodec, bool, Vec<u8>)> {
    extract_video_frames_with_codec(data, TsCodec::Unknown)
}

pub fn extract_video_frames_with_codec(
    data: &[u8],
    video_codec: TsCodec,
) -> Vec<(TsCodec, bool, Vec<u8>)> {
    let mut frames = Vec::new();
    let mut current_nal = Vec::new();
    let mut i = 0;

    while i + 4 <= data.len() {
        // Look for Annex B start code: 0x00 0x00 0x01 or 0x00 0x00 0x00 0x01
        let start_code_len = if data[i..i + 4] == [0x00, 0x00, 0x00, 0x01] {
            4
        } else if data[i..i + 3] == [0x00, 0x00, 0x01] {
            3
        } else {
            i += 1;
            continue;
        };

        // Save previous NAL unit
        if !current_nal.is_empty() {
            if let Some(frame) = classify_video_nal(&current_nal, video_codec) {
                frames.push(frame);
            }
            current_nal.clear();
        }

        // Move past start code
        i += start_code_len;

        // Collect until next start code
        while i < data.len() {
            if data[i] == 0x00
                && i + 1 < data.len()
                && data[i + 1] == 0x00
                && i + 2 < data.len()
                && (data[i + 2] == 0x01
                    || (data[i + 2] == 0x00 && i + 3 < data.len() && data[i + 3] == 0x01))
            {
                break;
            }
            current_nal.push(data[i]);
            i += 1;
        }
    }

    // Last NAL
    if !current_nal.is_empty() {
        if let Some(frame) = classify_video_nal(&current_nal, video_codec) {
            frames.push(frame);
        }
    }

    frames
}

fn classify_video_nal(nal: &[u8], video_codec: TsCodec) -> Option<(TsCodec, bool, Vec<u8>)> {
    if nal.is_empty() {
        return None;
    }
    let h264_nal_type = nal[0] & 0x1F;
    let h265_nal_type = (nal[0] & 0x7E) >> 1;

    let prefer_h264 = matches!(video_codec, TsCodec::H264)
        || (matches!(video_codec, TsCodec::Unknown)
            && matches!(h264_nal_type, 1..=9 | 12));
    let prefer_h265 = matches!(video_codec, TsCodec::H265)
        || (matches!(video_codec, TsCodec::Unknown) && !prefer_h264);

    if prefer_h264 {
        let is_h264_slice = h264_nal_type >= 1 && h264_nal_type <= 5;
        let is_h264_idr = h264_nal_type == 5;
        let is_h264_sps = h264_nal_type == 7;
        let is_h264_pps = h264_nal_type == 8;
        let is_h264_sei = h264_nal_type == 6;
        let is_h264_aud = h264_nal_type == 9;

        if is_h264_sps || is_h264_pps || is_h264_sei || is_h264_aud || is_h264_slice {
            let mut frame = Vec::with_capacity(4 + nal.len());
            frame.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            frame.extend_from_slice(nal);
            let is_key = is_h264_idr || is_h264_sps;
            return Some((TsCodec::H264, is_key, frame));
        }
    }

    if prefer_h265 {
        let is_h265_idr = h265_nal_type == 19 || h265_nal_type == 20;
        let is_h265_slice = (h265_nal_type >= 1 && h265_nal_type <= 9)
            || (h265_nal_type >= 16 && h265_nal_type <= 23);
        let is_h265_vps = h265_nal_type == 32;
        let is_h265_sps = h265_nal_type == 33;
        let is_h265_pps = h265_nal_type == 34;

        if is_h265_vps || is_h265_sps || is_h265_pps || is_h265_slice {
            let mut frame = Vec::with_capacity(4 + nal.len());
            frame.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            frame.extend_from_slice(nal);
            let is_key = is_h265_idr;
            return Some((TsCodec::H265, is_key, frame));
        }
    }

    None
}

/// Extract raw AAC data from ES data (strip ADTS headers if present).
pub fn extract_aac_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut offset = 0;

    while offset + 7 <= data.len() {
        // ADTS header: sync word 0xFFF
        if data[offset] == 0xFF && offset + 1 < data.len() && (data[offset + 1] & 0xF0) == 0xF0 {
            let header_dat = &data[offset..];
            if header_dat.len() < 7 {
                break;
            }
            let protection_absent = (header_dat[1] & 0x01) == 1;
            let header_len = if protection_absent { 7 } else { 9 };
            let frame_len = ((header_dat[3] as usize & 0x03) << 11)
                | ((header_dat[4] as usize) << 3)
                | ((header_dat[5] as usize & 0xE0) >> 5);

            if frame_len >= header_len && offset + frame_len <= data.len() {
                let raw_aac = &data[offset + header_len..offset + frame_len];
                if !raw_aac.is_empty() {
                    frames.push(raw_aac.to_vec());
                }
                offset += frame_len;
            } else {
                break;
            }
        } else {
            // No ADTS sync found, treat the whole buffer as raw AAC
            // (some streams use LATM or raw AAC)
            if !data.is_empty() {
                frames.push(data.to_vec());
            }
            break;
        }
    }

    // Fallback: if no ADTS frames found, return the whole data
    if frames.is_empty() && !data.is_empty() {
        frames.push(data.to_vec());
    }

    frames
}
