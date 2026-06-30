/// MPEG-TS (Transport Stream) muxer
/// Generates PAT, PMT, and PES packets for HLS segments.
use bytes::{Bytes, BytesMut, BufMut};
use tracing::debug;

use crate::core::{CodecType, MediaFrame};

/// AAC sample rate index (44100 Hz)
const AAC_SAMPLE_RATE_INDEX: u8 = 4;
const AAC_CHANNELS: u8 = 2;

/// Standard TS packet size
const TS_PACKET_SIZE: usize = 188;
/// Sync byte for TS packets
const SYNC_BYTE: u8 = 0x47;
/// PAT PID
const PAT_PID: u16 = 0x0000;
/// PMT PID
const PMT_PID: u16 = 0x1000;
/// Video PID (H264)
const VIDEO_PID: u16 = 0x100;
/// Audio PID (AAC)
const AUDIO_PID: u16 = 0x101;

/// CRC32 table for MPEG-TS (ISO 13818-1)
const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            if crc & 0x80000000 != 0 {
                crc = (crc << 1) ^ 0x04C11DB7;
            } else {
                crc <<= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &byte in data {
        let index = ((crc >> 24) ^ (byte as u32)) as usize & 0xFF;
        crc = (crc << 8) ^ CRC32_TABLE[index];
    }
    crc
}

/// MPEG-TS Muxer that produces TS packets from media frames
pub struct TsMuxer {
    continuity_counter_video: u8,
    continuity_counter_audio: u8,
    continuity_counter_pat: u8,
    continuity_counter_pmt: u8,
    /// PCR in 27MHz units
    pcr_clock: u64,
}

impl TsMuxer {
    pub fn new() -> Self {
        Self {
            continuity_counter_video: 0,
            continuity_counter_audio: 0,
            continuity_counter_pat: 0,
            continuity_counter_pmt: 0,
            pcr_clock: 0,
        }
    }

    /// Prepare for a new TS segment file. PAT/PMT are re-emitted; CC/PCR stay continuous.
    pub fn reset_for_new_segment(&mut self) {}

    /// Generate a PAT (Program Association Table) packet
    pub fn generate_pat(&mut self) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.push(0x00); // Pointer field
        payload.push(0x00); // Table ID
        let section_length: u16 = 13;
        payload.push(0xB0 | ((section_length >> 8) as u8 & 0x0F));
        payload.push((section_length & 0xFF) as u8);
        payload.extend_from_slice(&[0x00, 0x01]); // Transport stream ID
        payload.push(0xC1); // Version + current/next
        payload.push(0x00); // Section number
        payload.push(0x00); // Last section number
        payload.extend_from_slice(&[0x00, 0x01]); // Program number
        payload.push(((PMT_PID >> 8) as u8) & 0x1F); // PMT PID
        payload.push((PMT_PID & 0xFF) as u8);

        let crc = crc32(&payload[1..]);
        payload.extend_from_slice(&crc.to_be_bytes());

        ts_packet(PAT_PID, true, &payload, &mut self.continuity_counter_pat)
    }

    /// Generate a PMT (Program Map Table) packet
    pub fn generate_pmt(&mut self, has_video: bool, has_audio: bool) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.push(0x00); // Pointer field
        payload.push(0x02); // Table ID

        let mut stream_info_len: u16 = 0;
        if has_video { stream_info_len += 5; }
        if has_audio { stream_info_len += 5; }
        let section_length: u16 = 5 + 4 + stream_info_len + 4;

        payload.push(0xB0 | ((section_length >> 8) as u8 & 0x0F));
        payload.push((section_length & 0xFF) as u8);
        payload.extend_from_slice(&[0x00, 0x01]); // Program number
        payload.push(0xC1); // Version + current/next
        payload.push(0x00); // Section number
        payload.push(0x00); // Last section number

        // PCR PID
        if has_video {
            payload.push(((VIDEO_PID >> 8) as u8) & 0x1F);
            payload.push((VIDEO_PID & 0xFF) as u8);
        } else if has_audio {
            payload.push(((AUDIO_PID >> 8) as u8) & 0x1F);
            payload.push((AUDIO_PID & 0xFF) as u8);
        } else {
            payload.extend_from_slice(&[0x1F, 0xFF]);
        }
        payload.extend_from_slice(&[0xF0, 0x00]); // Program info length = 0

        if has_video {
            payload.push(0x1B); // H264
            payload.push(((VIDEO_PID >> 8) as u8) & 0x1F);
            payload.push((VIDEO_PID & 0xFF) as u8);
            payload.extend_from_slice(&[0xF0, 0x00]);
        }
        if has_audio {
            payload.push(0x0F); // AAC
            payload.push(((AUDIO_PID >> 8) as u8) & 0x1F);
            payload.push((AUDIO_PID & 0xFF) as u8);
            payload.extend_from_slice(&[0xF0, 0x00]);
        }

        let crc = crc32(&payload[1..]);
        payload.extend_from_slice(&crc.to_be_bytes());

        ts_packet(PMT_PID, true, &payload, &mut self.continuity_counter_pmt)
    }

    /// Generate PAT + PMT as a combined buffer
    pub fn generate_pat_pmt(&mut self, has_video: bool, has_audio: bool) -> Vec<u8> {
        let mut buf = self.generate_pat();
        buf.extend(self.generate_pmt(has_video, has_audio));
        buf
    }

    /// Wrap a media frame into PES packets inside TS packets
    pub fn frame_to_ts(&mut self, frame: &MediaFrame) -> Vec<u8> {
        let pid = match frame.codec {
            CodecType::H264 | CodecType::H265 => VIDEO_PID,
            CodecType::AAC | CodecType::Opus | CodecType::G711 => AUDIO_PID,
            _ => VIDEO_PID,
        };

        let is_video = matches!(frame.codec, CodecType::H264 | CodecType::H265);
        let payload = if matches!(frame.codec, CodecType::AAC) {
            wrap_aac_adts(&frame.data)
        } else {
            frame.data.to_vec()
        };
        let frame_for_pes = MediaFrame::new(
            frame.stream_id.clone(),
            frame.track_id,
            frame.timestamp,
            bytes::Bytes::from(payload),
            frame.is_keyframe,
            frame.codec,
        );
        let pes = build_pes(&frame_for_pes, is_video);

        let cc = if is_video {
            &mut self.continuity_counter_video
        } else {
            &mut self.continuity_counter_audio
        };
        pes_to_ts_packets(&pes, pid, is_video, cc, self.pcr_clock)
    }

    /// Update PCR clock
    pub fn update_pcr(&mut self, timestamp_ms: u64) {
        self.pcr_clock = timestamp_ms * 27000; // Convert ms to 27MHz
    }
}

impl Default for TsMuxer {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a PES (Packetized Elementary Stream) packet
fn build_pes(frame: &MediaFrame, is_video: bool) -> Vec<u8> {
    let mut pes = Vec::new();
    pes.push(0x00);
    pes.push(0x00);
    pes.push(0x01); // PES start code

    let stream_id = if is_video { 0xE0 } else { 0xC0 };
    pes.push(stream_id);

    let has_pts = true;
    // No B-frames: DTS equals PTS on every video access unit
    let has_dts = is_video;
    let optional_header_len = if has_pts && has_dts { 10 } else if has_pts { 5 } else { 0 };

    let pes_data_len = 3 + optional_header_len + frame.data.len();
    // Unbounded PES length avoids size mismatch for variable ADTS/H264 payloads
    pes.extend_from_slice(&[0x00, 0x00]);
    let _ = pes_data_len;

    let mut flags1: u8 = 0x80;
    if is_video { flags1 |= 0x04; }
    pes.push(flags1);

    let mut flags2: u8 = 0x00;
    if has_pts && has_dts { flags2 = 0xC0; }
    else if has_pts { flags2 = 0x80; }
    pes.push(flags2);

    pes.push(optional_header_len as u8);

    if has_pts {
        let pts_val = frame.timestamp * 90; // Convert ms to 90kHz
        let mut pts_bytes = [0u8; 5];
        pts_bytes[0] = 0x21 | (((pts_val >> 30) & 0x0E) as u8);
        pts_bytes[1] = ((pts_val >> 22) & 0xFF) as u8;
        pts_bytes[2] = 0x01 | (((pts_val >> 14) & 0xFE) as u8);
        pts_bytes[3] = ((pts_val >> 7) & 0xFF) as u8;
        pts_bytes[4] = 0x01 | (((pts_val << 1) & 0xFE) as u8);
        pes.extend_from_slice(&pts_bytes);
    }

    if has_pts && has_dts {
        let dts_val = frame.timestamp * 90;
        let mut dts_bytes = [0u8; 5];
        dts_bytes[0] = 0x31 | (((dts_val >> 30) & 0x0E) as u8);
        dts_bytes[1] = ((dts_val >> 22) & 0xFF) as u8;
        dts_bytes[2] = 0x01 | (((dts_val >> 14) & 0xFE) as u8);
        dts_bytes[3] = ((dts_val >> 7) & 0xFF) as u8;
        dts_bytes[4] = 0x01 | (((dts_val << 1) & 0xFE) as u8);
        pes.extend_from_slice(&dts_bytes);
    }

    pes.extend_from_slice(&frame.data);
    pes
}

/// Fragment a PES packet into TS packets (188 bytes each, with proper stuffing).
fn pes_to_ts_packets(pes: &[u8], pid: u16, is_video: bool, cc: &mut u8, pcr_clock: u64) -> Vec<u8> {
    let mut output = Vec::new();
    let mut offset = 0;
    let mut first = true;

    while offset < pes.len() {
        let mut packet = [0xFFu8; TS_PACKET_SIZE];
        packet[0] = SYNC_BYTE;
        packet[1] = ((pid >> 8) as u8) & 0x1F;
        packet[2] = (pid & 0xFF) as u8;
        *cc = (*cc + 1) & 0x0F;

        let mut payload_start = 4usize;

        if first {
            packet[1] |= 0x40; // PUSI
            if is_video {
                packet[3] = 0x30 | *cc;
                packet[4] = 7; // adaptation_field_length
                packet[5] = 0x10; // PCR
                let pcr = pcr_clock;
                packet[6] = ((pcr >> 25) & 0xFF) as u8;
                packet[7] = ((pcr >> 17) & 0xFF) as u8;
                packet[8] = ((pcr >> 9) & 0xFF) as u8;
                packet[9] = ((pcr >> 1) & 0xFF) as u8;
                packet[10] = ((pcr & 0x01) as u8) << 7 | 0x7E;
                packet[11] = 0x00;
                payload_start = 12;
            } else {
                packet[3] = 0x10 | *cc;
            }
            first = false;
        } else {
            packet[3] = 0x10 | *cc;
        }

        let capacity = TS_PACKET_SIZE - payload_start;
        let copy_len = std::cmp::min(capacity, pes.len() - offset);

        if copy_len < capacity {
            // Stuffing via adaptation field
            let adapt_len = capacity - copy_len;
            if payload_start == 4 {
                packet[3] = 0x30 | *cc;
                packet[4] = (adapt_len - 1) as u8;
                if adapt_len >= 2 {
                    packet[5] = 0x00;
                }
                payload_start = 4 + adapt_len;
            }
        }

        packet[payload_start..payload_start + copy_len]
            .copy_from_slice(&pes[offset..offset + copy_len]);
        offset += copy_len;
        output.extend_from_slice(&packet);
    }

    output
}

/// Generate basic TS packets from a payload (for PAT/PMT)
fn ts_packet(pid: u16, payload_start: bool, payload: &[u8], cc: &mut u8) -> Vec<u8> {
    let mut packets = Vec::new();
    let mut offset = 0;
    let mut first = true;

    while offset < payload.len() {
        let mut packet = [0u8; TS_PACKET_SIZE];
        packet[0] = SYNC_BYTE;
        packet[1] = ((pid >> 8) as u8) & 0x1F;
        packet[2] = (pid & 0xFF) as u8;
        *cc = (*cc + 1) & 0x0F;

        if first && payload_start {
            packet[1] |= 0x40;
        }

        let remaining = TS_PACKET_SIZE - 4;
        let copy_len = std::cmp::min(remaining, payload.len() - offset);

        if copy_len < remaining {
            packet[3] = 0x30 | *cc;
            let adaptation_len = remaining - copy_len;
            if adaptation_len == 1 {
                packet[4] = 0x00;
                let pos = 5;
                packet[pos..pos + copy_len].copy_from_slice(&payload[offset..offset + copy_len]);
            } else {
                packet[4] = (adaptation_len - 1) as u8;
                if adaptation_len >= 2 { packet[5] = 0x00; }
                for i in 6..4 + adaptation_len {
                    if i < TS_PACKET_SIZE - copy_len { packet[i] = 0xFF; }
                }
                let pos = 4 + adaptation_len;
                packet[pos..pos + copy_len].copy_from_slice(&payload[offset..offset + copy_len]);
            }
        } else {
            packet[3] = 0x10 | *cc;
            packet[4..4 + copy_len].copy_from_slice(&payload[offset..offset + copy_len]);
        }

        offset += copy_len;
        first = false;
        packets.extend_from_slice(&packet);
    }

    if packets.is_empty() {
        let mut packet = [0u8; TS_PACKET_SIZE];
        packet[0] = SYNC_BYTE;
        packet[1] = ((pid >> 8) as u8) & 0x1F;
        if payload_start { packet[1] |= 0x40; }
        packet[2] = (pid & 0xFF) as u8;
        *cc = (*cc + 1) & 0x0F;
        packet[3] = 0x10 | *cc;
        packets.extend_from_slice(&packet);
    }

    packets
}

/// Wrap raw AAC (RTMP-style, no ADTS) in a 7-byte ADTS header for MPEG-TS.
fn wrap_aac_adts(aac_raw: &[u8]) -> Vec<u8> {
    if aac_raw.is_empty() {
        return Vec::new();
    }
    // Already has ADTS sync word
    if aac_raw.len() >= 2 && aac_raw[0] == 0xFF && (aac_raw[1] & 0xF0) == 0xF0 {
        return aac_raw.to_vec();
    }

    let frame_len = aac_raw.len() + 7;
    let mut adts = Vec::with_capacity(frame_len);
    adts.push(0xFF);
    adts.push(0xF1); // MPEG-4, layer 0, no CRC
    adts.push(
        (0 << 6) | ((AAC_SAMPLE_RATE_INDEX & 0x0F) << 2) | ((AAC_CHANNELS >> 2) & 0x01),
    );
    adts.push(((AAC_CHANNELS & 0x03) << 6) | ((frame_len >> 11) as u8));
    adts.push(((frame_len >> 3) & 0xFF) as u8);
    adts.push((((frame_len & 0x07) as u8) << 5) | 0x1F);
    adts.push(0xFC);
    adts.extend_from_slice(aac_raw);
    adts
}
