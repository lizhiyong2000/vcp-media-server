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
    Mpeg2Video,
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
        let program_info_len = ((payload[10] as usize & 0x0F) << 8) | payload[11] as usize;
        let mut offset = 12 + program_info_len;

        // Exclude CRC_32 (last 4 bytes) from stream entry parsing
        let pmt_data_end = section_end.saturating_sub(4);
        while offset + 5 <= pmt_data_end {
            let stream_type = payload[offset];
            let elementary_pid =
                ((payload[offset + 1] as u16 & 0x1F) << 8) | payload[offset + 2] as u16;
            let es_info_len =
                ((payload[offset + 3] as usize & 0x0F) << 8) | payload[offset + 4] as usize;

            match stream_type {
                0x02 => {
                    // MPEG-2 video is common in legacy satellite/cable feeds.
                    // The current media pipeline cannot publish it as H264/H265.
                    debug!("[TS] Found MPEG2 video PID={}", elementary_pid);
                    video_pid = Some(elementary_pid);
                    video_codec = TsCodec::Mpeg2Video;
                }
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
    extract_es_data_with_codec(packets, video_pid, audio_pid, TsCodec::Unknown)
}

pub fn extract_es_data_with_codec(
    packets: &[&[u8]],
    video_pid: u16,
    audio_pid: u16,
    video_codec: TsCodec,
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
                process_pes_packet(&buf, pid, video_pid, video_codec, &mut es_map);
            }
        }

        pes_buf
            .entry(pid)
            .or_default()
            .extend_from_slice(raw_payload);
    }

    // Flush remaining
    for (pid, buf) in pes_buf.drain() {
        process_pes_packet(&buf, pid, video_pid, video_codec, &mut es_map);
    }

    es_map
}

fn process_pes_packet(
    buf: &[u8],
    pid: u16,
    video_pid: u16,
    video_codec: TsCodec,
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
    match pes_data
        .windows(4)
        .position(|w| w == [0x00, 0x00, 0x01, b'\xE0'] || w == [0x00, 0x00, 0x01, b'\xC0'])
    {
        Some(offset) if offset < 3 => {
            if offset > 0 {
                pes_data = &pes_data[offset..];
            }
        }
        // No PES header found, treat as continuation data
        _ => {
            let codec = pid_to_codec(pid, video_pid, video_codec);
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

    let codec = pid_to_codec(pid, video_pid, video_codec);
    let entry = es_map.entry(pid).or_insert(TsElementaryData {
        codec,
        data: Vec::new(),
    });
    entry.data.extend_from_slice(&pes_data[data_offset..]);
}

fn pid_to_codec(pid: u16, video_pid: u16, video_codec: TsCodec) -> TsCodec {
    if pid == video_pid {
        video_codec
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

fn ts_payload(pkt: &[u8]) -> Option<&[u8]> {
    if pkt.len() != TS_PACKET_SIZE {
        return None;
    }
    let has_payload = (pkt[3] & 0x10) != 0;
    if !has_payload {
        return None;
    }
    let offset = ts_payload_offset(pkt);
    if offset >= TS_PACKET_SIZE {
        return None;
    }
    Some(&pkt[offset..])
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
    if matches!(video_codec, TsCodec::Mpeg2Video | TsCodec::AAC) {
        return Vec::new();
    }

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
        || (matches!(video_codec, TsCodec::Unknown) && matches!(h264_nal_type, 1..=9 | 12));
    let prefer_h265 = matches!(video_codec, TsCodec::H265)
        || (matches!(video_codec, TsCodec::Unknown) && !prefer_h264);

    if prefer_h264 {
        let h264_nal_ref_idc = (nal[0] >> 5) & 0x03;
        let is_h264_slice = h264_nal_type >= 1 && h264_nal_type <= 5;
        let is_h264_idr = h264_nal_type == 5;
        let is_h264_sps = h264_nal_type == 7;
        let is_h264_pps = h264_nal_type == 8;
        let is_h264_sei = h264_nal_type == 6;
        let is_h264_aud = h264_nal_type == 9;

        if (is_h264_idr || is_h264_sps || is_h264_pps) && h264_nal_ref_idc == 0 {
            return None;
        }

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

/// Incremental MPEG-TS decoder for streaming HLS segment downloads.
/// Feeds TS packets as bytes arrive and emits complete video NAL units without
/// waiting for the full segment.
pub struct TsStreamDecoder {
    raw: Vec<u8>,
    sync_offset: Option<usize>,
    consumed_packets: usize,
    video_pid: Option<u16>,
    audio_pid: Option<u16>,
    video_codec: TsCodec,
    pes_buf: HashMap<u16, Vec<u8>>,
    video_es: Vec<u8>,
    nal_scan_pos: usize,
    audio_es: Vec<u8>,
}

impl TsStreamDecoder {
    pub fn new(is_h265_hint: bool) -> Self {
        Self {
            raw: Vec::new(),
            sync_offset: None,
            consumed_packets: 0,
            video_pid: None,
            audio_pid: None,
            video_codec: if is_h265_hint {
                TsCodec::H265
            } else {
                TsCodec::H264
            },
            pes_buf: HashMap::new(),
            video_es: Vec::new(),
            nal_scan_pos: 0,
            audio_es: Vec::new(),
        }
    }

    pub fn video_codec(&self) -> TsCodec {
        self.video_codec
    }

    pub fn consumed_packets(&self) -> usize {
        self.consumed_packets
    }

    pub fn raw_len(&self) -> usize {
        self.raw.len()
    }

    pub fn has_sync(&self) -> bool {
        self.sync_offset.is_some()
    }

    pub fn video_pid(&self) -> Option<u16> {
        self.video_pid
    }

    pub fn audio_pid(&self) -> Option<u16> {
        self.audio_pid
    }

    pub fn audio_es(&self) -> &[u8] {
        &self.audio_es
    }

    pub fn append(&mut self, chunk: &[u8]) {
        self.raw.extend_from_slice(chunk);
    }

    pub fn has_pending_packets(&self) -> bool {
        let Some(sync) = self.sync_offset else {
            return self.raw.len() >= TS_PACKET_SIZE * 2;
        };
        let start = sync + self.consumed_packets * TS_PACKET_SIZE;
        start + TS_PACKET_SIZE <= self.raw.len() && self.raw[start] == TS_SYNC_BYTE
    }

    /// Process up to `max_packets` TS packets and return any complete video NAL units.
    pub fn process_packets(&mut self, max_packets: usize) -> Vec<(TsCodec, bool, Vec<u8>)> {
        self.ensure_sync();
        self.try_discover_program();
        let mut processed = 0usize;
        while processed < max_packets && self.feed_next_packet() {
            processed += 1;
        }
        self.compact_raw_if_needed();
        self.drain_complete_video_nals(false)
    }

    /// Feed newly downloaded bytes; returns any complete video NAL units found.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<(TsCodec, bool, Vec<u8>)> {
        self.append(chunk);
        let mut out = Vec::new();
        loop {
            let batch = self.process_packets(128);
            if batch.is_empty() && !self.has_pending_packets() {
                break;
            }
            out.extend(batch);
            if !self.has_pending_packets() {
                break;
            }
        }
        out
    }

    /// Flush remaining PES buffers and emit the trailing video NAL (if any).
    pub fn finish(&mut self) -> Vec<(TsCodec, bool, Vec<u8>)> {
        self.flush_pes_buffers();
        self.drain_complete_video_nals(true)
    }

    fn ensure_sync(&mut self) {
        if self.sync_offset.is_some() {
            return;
        }
        if self.raw.len() <= TS_PACKET_SIZE {
            return;
        }
        let scan_limit = self.raw.len() - TS_PACKET_SIZE - 1;
        for offset in 0..=scan_limit {
            if self.raw[offset] != TS_SYNC_BYTE {
                continue;
            }
            if self.raw[offset + TS_PACKET_SIZE] == TS_SYNC_BYTE {
                self.sync_offset = Some(offset);
                return;
            }
        }
    }

    fn try_discover_program(&mut self) {
        if self.video_pid.is_some() {
            return;
        }
        let sync = match self.sync_offset {
            Some(s) => s,
            None => return,
        };
        let packets = collect_ts_packets_prefix(&self.raw, sync, 200);
        if packets.is_empty() {
            return;
        }
        if packets.len() >= 3 {
            let pmt_pids = parse_pat(&packets);
            for &pmt_pid in &pmt_pids {
                let info = parse_pmt_info(&packets, pmt_pid);
                if info.video_pid.is_none() {
                    continue;
                }
                self.video_pid = info.video_pid;
                self.audio_pid = info.audio_pid;
                if info.video_codec != TsCodec::Unknown {
                    self.video_codec = info.video_codec;
                }
                return;
            }
        }
        let (video_pid, audio_pid) = infer_pids_from_pes(&packets);
        if self.video_pid.is_none() {
            self.video_pid = video_pid;
        }
        if self.audio_pid.is_none() {
            self.audio_pid = audio_pid;
        }
    }

    fn feed_next_packet(&mut self) -> bool {
        let sync = match self.sync_offset {
            Some(s) => s,
            None => return false,
        };
        let start = sync + self.consumed_packets * TS_PACKET_SIZE;
        if start + TS_PACKET_SIZE > self.raw.len() || self.raw[start] != TS_SYNC_BYTE {
            return false;
        }
        let mut pkt = [0u8; TS_PACKET_SIZE];
        pkt.copy_from_slice(&self.raw[start..start + TS_PACKET_SIZE]);
        self.consumed_packets += 1;
        self.feed_ts_packet(&pkt);
        true
    }

    fn compact_raw_if_needed(&mut self) {
        const COMPACT_THRESHOLD: usize = 256 * 1024;
        let Some(sync) = self.sync_offset else {
            return;
        };
        let consumed_end = sync + self.consumed_packets * TS_PACKET_SIZE;
        if consumed_end < COMPACT_THRESHOLD {
            return;
        }
        self.raw.drain(0..consumed_end);
        self.sync_offset = Some(0);
        self.consumed_packets = 0;
    }

    fn feed_ts_packet(&mut self, pkt: &[u8]) {
        let video_pid = match self.video_pid {
            Some(pid) => pid,
            None => return,
        };
        let audio_pid = self.audio_pid.unwrap_or(0xFFFF);
        let pid = ts_pid(pkt);
        if pid != video_pid && pid != audio_pid {
            return;
        }

        let pusi = (pkt[1] & 0x40) != 0;
        let Some(raw_payload) = ts_payload(pkt) else {
            return;
        };

        if pusi {
            if let Some(buf) = self.pes_buf.remove(&pid) {
                self.append_pes_payload(&buf, pid, video_pid);
            }
        }
        self.pes_buf
            .entry(pid)
            .or_default()
            .extend_from_slice(raw_payload);
    }

    fn flush_pes_buffers(&mut self) {
        let video_pid = self.video_pid.unwrap_or(0xFFFF);
        let pending: Vec<(u16, Vec<u8>)> = self.pes_buf.drain().collect();
        for (pid, buf) in pending {
            self.append_pes_payload(&buf, pid, video_pid);
        }
    }

    fn append_pes_payload(&mut self, buf: &[u8], pid: u16, video_pid: u16) {
        if buf.len() < 6 {
            return;
        }

        let mut pes_data = buf;
        match pes_data
            .windows(4)
            .position(|w| w == [0x00, 0x00, 0x01, b'\xE0'] || w == [0x00, 0x00, 0x01, b'\xC0'])
        {
            Some(offset) if offset < 3 => {
                if offset > 0 {
                    pes_data = &pes_data[offset..];
                }
            }
            _ => {
                if pid == video_pid {
                    self.video_es.extend_from_slice(buf);
                } else {
                    self.audio_es.extend_from_slice(buf);
                }
                return;
            }
        }

        if pes_data.len() < 9 {
            return;
        }
        let pes_header_data_len = pes_data[8] as usize;
        let data_offset = 9 + pes_header_data_len;
        if data_offset >= pes_data.len() {
            return;
        }

        let payload = &pes_data[data_offset..];
        if pid == video_pid {
            self.video_es.extend_from_slice(payload);
        } else {
            self.audio_es.extend_from_slice(payload);
        }
    }

    fn drain_complete_video_nals(&mut self, emit_tail: bool) -> Vec<(TsCodec, bool, Vec<u8>)> {
        drain_complete_annex_b_nals(
            &self.video_es,
            &mut self.nal_scan_pos,
            self.video_codec,
            emit_tail,
        )
    }
}

fn collect_ts_packets_prefix<'a>(raw: &'a [u8], sync: usize, max_packets: usize) -> Vec<&'a [u8]> {
    let mut packets = Vec::with_capacity(max_packets.min(64));
    let mut pos = sync;
    for _ in 0..max_packets {
        if pos + TS_PACKET_SIZE > raw.len() {
            break;
        }
        if raw[pos] != TS_SYNC_BYTE {
            break;
        }
        packets.push(&raw[pos..pos + TS_PACKET_SIZE]);
        pos += TS_PACKET_SIZE;
    }
    packets
}

fn infer_pids_from_pes(packets: &[&[u8]]) -> (Option<u16>, Option<u16>) {
    let mut video_pid = None;
    let mut audio_pid = None;
    for pkt in packets {
        if pkt.len() != TS_PACKET_SIZE || pkt[0] != TS_SYNC_BYTE {
            continue;
        }
        let pusi = (pkt[1] & 0x40) != 0;
        if !pusi {
            continue;
        }
        let Some(payload) = ts_payload(pkt) else {
            continue;
        };
        if payload.len() < 4 || payload[0..3] != [0x00, 0x00, 0x01] {
            continue;
        }
        let pid = ts_pid(pkt);
        match payload[3] {
            0xE0..=0xEF if video_pid.is_none() => {
                video_pid = Some(pid);
            }
            0xC0..=0xDF if audio_pid.is_none() => {
                audio_pid = Some(pid);
            }
            _ => {}
        }
        if video_pid.is_some() && audio_pid.is_some() {
            break;
        }
    }
    (video_pid, audio_pid)
}

fn drain_complete_annex_b_nals(
    data: &[u8],
    scan_from: &mut usize,
    video_codec: TsCodec,
    emit_tail: bool,
) -> Vec<(TsCodec, bool, Vec<u8>)> {
    let mut frames = Vec::new();
    if *scan_from >= data.len() {
        return frames;
    }

    let mut i = *scan_from;
    let mut current_start: Option<usize> = None;

    while i + 3 < data.len() {
        let start_code_len = if i + 4 <= data.len() && data[i..i + 4] == [0x00, 0x00, 0x00, 0x01] {
            4
        } else if data[i..i + 3] == [0x00, 0x00, 0x01] {
            3
        } else {
            i += 1;
            continue;
        };

        if let Some(start) = current_start {
            let nal_with_prefix = &data[start..i];
            if let Some(frame) = classify_video_nal_from_annex_b(nal_with_prefix, video_codec) {
                frames.push(frame);
            }
        }
        current_start = Some(i);
        i += start_code_len;
    }

    if emit_tail {
        if let Some(start) = current_start {
            let nal_with_prefix = &data[start..];
            if let Some(frame) = classify_video_nal_from_annex_b(nal_with_prefix, video_codec) {
                frames.push(frame);
            }
        }
        *scan_from = data.len();
    } else if let Some(start) = current_start {
        *scan_from = start;
    } else {
        *scan_from = i;
    }

    frames
}

fn classify_video_nal_from_annex_b(
    nal_with_prefix: &[u8],
    video_codec: TsCodec,
) -> Option<(TsCodec, bool, Vec<u8>)> {
    let nal = strip_annex_b_start_code(nal_with_prefix)?;
    classify_video_nal(nal, video_codec).map(|(codec, is_key, mut frame)| {
        if frame.is_empty() {
            frame = Vec::with_capacity(4 + nal.len());
            frame.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            frame.extend_from_slice(nal);
        }
        (codec, is_key, frame)
    })
}

fn strip_annex_b_start_code(data: &[u8]) -> Option<&[u8]> {
    if data.len() >= 4 && data[0..4] == [0x00, 0x00, 0x00, 0x01] {
        return Some(&data[4..]);
    }
    if data.len() >= 3 && data[0..3] == [0x00, 0x00, 0x01] {
        return Some(&data[3..]);
    }
    None
}

#[cfg(test)]
mod stream_decoder_tests {
    use super::*;

    fn pes_ts_packet(pid: u16, stream_id: u8, payload: &[u8]) -> [u8; TS_PACKET_SIZE] {
        let mut pkt = [0xFF; TS_PACKET_SIZE];
        pkt[0] = TS_SYNC_BYTE;
        pkt[1] = 0x40 | ((pid >> 8) as u8 & 0x1F);
        pkt[2] = pid as u8;
        pkt[3] = 0x10;
        let mut pos = 4;
        let pes_header = [0x00, 0x00, 0x01, stream_id, 0x00, 0x00, 0x80, 0x00, 0x00];
        pkt[pos..pos + pes_header.len()].copy_from_slice(&pes_header);
        pos += pes_header.len();
        let copy_len = payload.len().min(TS_PACKET_SIZE - pos);
        pkt[pos..pos + copy_len].copy_from_slice(&payload[..copy_len]);
        pkt
    }

    #[test]
    fn drain_complete_nals_waits_for_next_start_code() {
        let data = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1f, 0x00, 0x00, 0x00, 0x01, 0x68, 0xce,
            0x3c, 0x80,
        ];
        let mut scan = 0usize;
        let first = drain_complete_annex_b_nals(&data, &mut scan, TsCodec::H264, false);
        assert_eq!(first.len(), 1);
        assert!(first[0].1);
        assert_eq!(scan, 8);
    }

    #[test]
    fn stream_decoder_emits_nals_from_incremental_annex_b_feed() {
        const DATA: &[u8] = &[
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1f, 0x00, 0x00, 0x00, 0x01, 0x68, 0xce,
            0x3c, 0x80, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84,
        ];
        let mut decoder = TsStreamDecoder::new(false);
        decoder.video_es.extend_from_slice(DATA);
        let mut all = decoder.drain_complete_video_nals(false);
        all.extend(decoder.finish());
        assert!(all.iter().any(|(_, is_key, _)| *is_key));
    }

    #[test]
    fn h264_invalid_reference_parameter_and_idr_nals_are_ignored() {
        assert!(classify_video_nal(&[0x07, 0x42, 0x00, 0x1f], TsCodec::H264).is_none());
        assert!(classify_video_nal(&[0x08, 0xce, 0x3c, 0x80], TsCodec::H264).is_none());
        assert!(classify_video_nal(&[0x05, 0x88, 0x84], TsCodec::H264).is_none());

        assert!(classify_video_nal(&[0x67, 0x42, 0x00, 0x1f], TsCodec::H264).is_some());
        assert!(classify_video_nal(&[0x68, 0xce, 0x3c, 0x80], TsCodec::H264).is_some());
        assert!(classify_video_nal(&[0x65, 0x88, 0x84], TsCodec::H264).is_some());
    }

    #[test]
    fn mpeg2_video_payload_is_not_scanned_as_h264_annex_b() {
        let mpeg2_es_with_start_codes = [
            0x00, 0x00, 0x01, 0x00, 0x12, 0x34, 0x00, 0x00, 0x01, 0xb3, 0x2d, 0x02, 0x40, 0x00,
            0x00, 0x01, 0xb8, 0x80,
        ];

        assert!(
            extract_video_frames_with_codec(&mpeg2_es_with_start_codes, TsCodec::Mpeg2Video)
                .is_empty()
        );
    }

    #[test]
    fn no_sync_means_no_pending_packets() {
        let mut decoder = TsStreamDecoder::new(false);
        decoder.append(&[0xFF, 0xFF]);
        assert!(!decoder.has_pending_packets());
        decoder.append(&[0xFF; TS_PACKET_SIZE]);
        assert!(!decoder.has_pending_packets());
    }

    #[test]
    fn stream_decoder_discovers_video_pid_from_pes_when_pat_pmt_missing() {
        let payload = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1f, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88,
            0x84,
        ];
        let pkt = pes_ts_packet(0x0100, 0xE0, &payload);
        let next_pkt = pes_ts_packet(0x0100, 0xE0, &[]);
        let mut decoder = TsStreamDecoder::new(false);
        decoder.append(&pkt);
        decoder.append(&next_pkt);

        let mut nals = decoder.process_packets(8);
        nals.extend(decoder.finish());

        assert_eq!(decoder.video_pid(), Some(0x0100));
        assert!(nals.iter().any(|(_, is_key, _)| *is_key));
    }

    #[test]
    fn stream_decoder_drains_multi_mb_segment() {
        let path = "/tmp/test.ts";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let data = std::fs::read(path).expect("read segment");
        assert!(
            data.len() > 1024 * 1024,
            "fixture should be a large TS segment"
        );

        let mut decoder = TsStreamDecoder::new(false);
        decoder.append(&data);
        let mut total_nals = 0usize;
        let mut has_key = false;
        for _ in 0..512 {
            if !decoder.has_pending_packets() {
                break;
            }
            let before = decoder.consumed_packets();
            let nals = decoder.process_packets(128);
            total_nals += nals.len();
            has_key |= nals.iter().any(|(_, is_key, _)| *is_key);
            if decoder.consumed_packets() == before && nals.is_empty() {
                break;
            }
        }
        let tail = decoder.finish();
        total_nals += tail.len();
        has_key |= tail.iter().any(|(_, is_key, _)| *is_key);

        assert!(
            decoder.video_pid().is_some(),
            "video pid should be discovered"
        );
        assert!(total_nals > 0, "expected video nals from large segment");
        assert!(has_key, "expected at least one keyframe nal");
        assert!(
            !decoder.has_pending_packets(),
            "segment should be fully drained"
        );
    }
}
