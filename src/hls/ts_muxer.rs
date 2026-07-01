/// MPEG-TS (Transport Stream) muxer
/// Generates PAT, PMT, and PES packets for HLS segments.
use bytes::Bytes;

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

/// MPEG-TS adaptation-field flags
const AF_DISCONTINUITY: u8 = 0x80;
const AF_PCR: u8 = 0x10;

/// MPEG-TS Muxer that produces TS packets from media frames
pub struct TsMuxer {
    continuity_counter_video: u8,
    continuity_counter_audio: u8,
    continuity_counter_pat: u8,
    continuity_counter_pmt: u8,
    /// PCR in 27MHz units
    pcr_clock: u64,
    /// Set on the first PAT packet of a segment (resets demuxer CC/PCR state).
    segment_discontinuity: bool,
}

impl TsMuxer {
    pub fn new() -> Self {
        Self {
            continuity_counter_video: 0,
            continuity_counter_audio: 0,
            continuity_counter_pat: 0,
            continuity_counter_pmt: 0,
            pcr_clock: 0,
            segment_discontinuity: false,
        }
    }

    /// Signal a timestamp/CC reset at the next segment header (after HLS segment split).
    pub fn mark_segment_discontinuity(&mut self) {
        self.segment_discontinuity = true;
    }

    /// New segment file: keep CC/PCR session-continuous (ffmpeg HLS demux expectation).
    pub fn reset_for_new_segment(&mut self) {
        // Do not reset pcr_clock or CC — segment files share one timeline.
    }

    /// Full reset after lag snap (true timeline discontinuity).
    pub fn hard_reset(&mut self) {
        *self = Self::new();
    }

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

        let discontinuity = self.take_segment_discontinuity();
        ts_packet(
            PAT_PID,
            true,
            &payload,
            &mut self.continuity_counter_pat,
            discontinuity,
        )
    }

    /// Generate a PMT (Program Map Table) packet
    pub fn generate_pmt(&mut self, has_video: bool, has_audio: bool) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.push(0x00); // Pointer field
        payload.push(0x02); // Table ID

        let mut stream_info_len: u16 = 0;
        if has_video {
            stream_info_len += 5;
        }
        if has_audio {
            stream_info_len += 5;
        }
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

        ts_packet(PMT_PID, true, &payload, &mut self.continuity_counter_pmt, false)
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
        } else if is_video {
            prepare_h264_au_for_ts(&frame.data)
        } else {
            frame.data.to_vec()
        };
        if payload.is_empty() {
            return Vec::new();
        }
        let frame_for_pes = MediaFrame::new(
            frame.stream_id.clone(),
            frame.track_id,
            frame.timestamp,
            Bytes::from(payload),
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

    fn take_segment_discontinuity(&mut self) -> bool {
        std::mem::take(&mut self.segment_discontinuity)
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

/// Prepend H264 AUD if missing; helps TS demuxers find access-unit boundaries.
fn prepare_h264_au_for_ts(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    if starts_with_aud(data) {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(6 + data.len());
    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x09, 0xF0]);
    out.extend_from_slice(data);
    out
}

fn starts_with_aud(data: &[u8]) -> bool {
    if data.starts_with(&[0x00, 0x00, 0x00, 0x01, 0x09]) {
        return true;
    }
    data.starts_with(&[0x00, 0x00, 0x01, 0x09])
}

/// Build a PES (Packetized Elementary Stream) packet
fn build_pes(frame: &MediaFrame, is_video: bool) -> Vec<u8> {
    let mut pes = Vec::new();
    pes.push(0x00);
    pes.push(0x00);
    pes.push(0x01); // PES start code

    let stream_id = if is_video { 0xE0 } else { 0xC0 };
    pes.push(stream_id);

    // PES_packet_length covers the 3-byte fixed header (flags + pes_header_data_length)
    // plus optional fields (PTS = 5 bytes) and elementary-stream payload.
    const PES_FIXED_HEADER_LEN: usize = 3;
    const PTS_FIELD_LEN: usize = 5;
    let pes_body_len = PES_FIXED_HEADER_LEN + PTS_FIELD_LEN + frame.data.len();

    if pes_body_len <= 0xFFFF {
        pes.push(((pes_body_len >> 8) & 0xFF) as u8);
        pes.push((pes_body_len & 0xFF) as u8);
    } else {
        // Unbounded PES (required when payload exceeds 65535 bytes).
        pes.extend_from_slice(&[0x00, 0x00]);
    }

    // First byte must begin with '10' (0x80..0xBF). Do not set bit 6 — that breaks the prefix.
    let mut flags1: u8 = 0x80;
    if is_video {
        flags1 |= 0x04; // data_alignment_indicator
    }
    pes.push(flags1);
    pes.push(0x80); // '10' + PTS only
    pes.push(PTS_FIELD_LEN as u8);

    write_pes_timestamp(&mut pes, 0x20, frame.timestamp);
    pes.extend_from_slice(&frame.data);
    pes
}

fn write_pes_timestamp(pes: &mut Vec<u8>, marker: u8, timestamp_ms: u64) {
    let pts_val = timestamp_ms.saturating_mul(90);
    pes.push(marker | 0x01 | (((pts_val >> 30) & 0x0E) as u8));
    pes.push(((pts_val >> 22) & 0xFF) as u8);
    pes.push(0x01 | (((pts_val >> 14) & 0xFE) as u8));
    pes.push(((pts_val >> 7) & 0xFF) as u8);
    pes.push(0x01 | (((pts_val << 1) & 0xFE) as u8));
}

/// Write a minimal adaptation field (discontinuity and/or PCR) before payload.
fn write_adaptation_prefix(
    packet: &mut [u8; TS_PACKET_SIZE],
    cc_val: u8,
    payload_start: &mut usize,
    discontinuity: bool,
    pcr: Option<u64>,
) {
    let pcr_flag = pcr.is_some();
    let af_len = if pcr_flag { 7u8 } else { 0u8 };
    let mut flags = 0u8;
    if discontinuity {
        flags |= AF_DISCONTINUITY;
    }
    if pcr_flag {
        flags |= AF_PCR;
    }

    packet[3] = 0x30 | cc_val;
    packet[4] = af_len;
    packet[5] = flags;
    let mut off = 6usize;
    if let Some(pcr_val) = pcr {
        write_pcr(&mut packet[off..off + 6], pcr_val);
        off += 6;
    }
    *payload_start = off;
}

/// Fragment a PES packet into TS packets (188 bytes each, with proper stuffing).
fn pes_to_ts_packets(pes: &[u8], pid: u16, is_video: bool, cc: &mut u8, pcr_clock: u64) -> Vec<u8> {
    let mut output = Vec::new();
    let mut offset = 0;
    let mut first = true;

    while offset < pes.len() {
        let cc_val = *cc;
        let mut packet = [0xFFu8; TS_PACKET_SIZE];
        packet[0] = SYNC_BYTE;
        packet[1] = ((pid >> 8) as u8) & 0x1F;
        packet[2] = (pid & 0xFF) as u8;

        let mut payload_start = 4usize;

        if first {
            packet[1] |= 0x40; // payload_unit_start_indicator
            if is_video {
                write_adaptation_prefix(
                    &mut packet,
                    cc_val,
                    &mut payload_start,
                    false,
                    Some(pcr_clock),
                );
            } else {
                packet[3] = 0x10 | cc_val;
            }
            first = false;
        } else {
            packet[3] = 0x10 | cc_val;
        }

        let mut room = TS_PACKET_SIZE.saturating_sub(payload_start);
        let mut copy_len = pes.len().saturating_sub(offset).min(room);

        if copy_len < room {
            // Need stuffing via adaptation field before payload
            let stuffing = room - copy_len;
            let af_len = stuffing.saturating_sub(1);
            if payload_start == 4 {
                packet[3] = 0x30 | cc_val;
                packet[4] = af_len as u8;
                if af_len > 0 {
                    packet[5] = 0x00;
                    for b in packet[6..4 + stuffing].iter_mut() {
                        *b = 0xFF;
                    }
                }
                payload_start = 4 + stuffing;
                room = TS_PACKET_SIZE.saturating_sub(payload_start);
                copy_len = pes.len().saturating_sub(offset).min(room);
            }
        }

        packet[payload_start..payload_start + copy_len]
            .copy_from_slice(&pes[offset..offset + copy_len]);
        offset += copy_len;

        *cc = (cc_val + 1) & 0x0F;
        output.extend_from_slice(&packet);
    }

    output
}

fn write_pcr(out: &mut [u8], pcr_base: u64) {
    out[0] = ((pcr_base >> 25) & 0xFF) as u8;
    out[1] = ((pcr_base >> 17) & 0xFF) as u8;
    out[2] = ((pcr_base >> 9) & 0xFF) as u8;
    out[3] = ((pcr_base >> 1) & 0xFF) as u8;
    out[4] = ((pcr_base & 0x01) as u8) << 7 | 0x7E;
    out[5] = 0x00;
}

/// Generate basic TS packets from a payload (for PAT/PMT)
fn ts_packet(
    pid: u16,
    payload_start: bool,
    payload: &[u8],
    cc: &mut u8,
    discontinuity: bool,
) -> Vec<u8> {
    let mut packets = Vec::new();
    let mut offset = 0;
    let mut first = true;

    while offset < payload.len() {
        let cc_val = *cc;
        let mut packet = [0xFFu8; TS_PACKET_SIZE];
        packet[0] = SYNC_BYTE;
        packet[1] = ((pid >> 8) as u8) & 0x1F;
        packet[2] = (pid & 0xFF) as u8;

        if first && payload_start {
            packet[1] |= 0x40;
        }

        let mut payload_start_off = 4usize;
        let remaining = TS_PACKET_SIZE - payload_start_off;
        let mut copy_len = std::cmp::min(remaining, payload.len() - offset);
        let need_adaptation = copy_len < remaining || (first && discontinuity);

        if need_adaptation {
            let mut stuffing = remaining.saturating_sub(copy_len);
            if first && discontinuity && stuffing < 2 {
                stuffing = 2;
            }
            let af_len = stuffing.saturating_sub(1);
            packet[3] = 0x30 | cc_val;
            packet[4] = af_len as u8;
            packet[5] = if first && discontinuity {
                AF_DISCONTINUITY
            } else {
                0x00
            };
            if af_len > 0 {
                for b in packet[6..4 + stuffing].iter_mut() {
                    *b = 0xFF;
                }
            }
            payload_start_off = 4 + stuffing;
            copy_len = std::cmp::min(payload.len() - offset, TS_PACKET_SIZE - payload_start_off);
        } else {
            packet[3] = 0x10 | cc_val;
        }

        packet[payload_start_off..payload_start_off + copy_len]
            .copy_from_slice(&payload[offset..offset + copy_len]);
        offset += copy_len;
        first = false;
        *cc = (cc_val + 1) & 0x0F;
        packets.extend_from_slice(&packet);
    }

    if packets.is_empty() {
        let cc_val = *cc;
        let mut packet = [0u8; TS_PACKET_SIZE];
        packet[0] = SYNC_BYTE;
        packet[1] = ((pid >> 8) as u8) & 0x1F;
        if payload_start {
            packet[1] |= 0x40;
        }
        packet[2] = (pid & 0xFF) as u8;
        packet[3] = 0x10 | cc_val;
        *cc = (cc_val + 1) & 0x0F;
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
    adts.push((0 << 6) | ((AAC_SAMPLE_RATE_INDEX & 0x0F) << 2) | ((AAC_CHANNELS >> 2) & 0x01));
    adts.push(((AAC_CHANNELS & 0x03) << 6) | ((frame_len >> 11) as u8));
    adts.push(((frame_len >> 3) & 0xFF) as u8);
    adts.push((((frame_len & 0x07) as u8) << 5) | 0x1F);
    adts.push(0xFC);
    adts.extend_from_slice(aac_raw);
    adts
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use crate::core::{CodecType, MediaFrame};

    #[test]
    fn pes_timestamp_encoding_90khz() {
        let mut pes = Vec::new();
        write_pes_timestamp(&mut pes, 0x20, 1000);
        assert_eq!(pes.len(), 5);
    }

    #[test]
    fn pes_packet_length_matches_body() {
        let payload = vec![0xABu8; 200];
        let frame = MediaFrame::new(
            "s".into(),
            0,
            1000,
            Bytes::from(payload.clone()),
            false,
            CodecType::H264,
        );
        let pes = build_pes(&frame, true);
        assert_eq!(&pes[0..3], &[0x00, 0x00, 0x01]);
        assert_eq!(pes[3], 0xE0);
        let declared = ((pes[4] as usize) << 8) | pes[5] as usize;
        let body = &pes[6..];
        assert_eq!(declared, body.len());
        assert_eq!(pes[6] & 0xC0, 0x80, "PES header must start with 10");
        assert_eq!(pes[7], 0x80);
        assert_eq!(pes[8], 5);
        assert_eq!(&pes[14..], &payload[..]);
    }

    #[test]
    fn pat_first_packet_sets_discontinuity_indicator() {
        let mut muxer = TsMuxer::new();
        muxer.mark_segment_discontinuity();
        let pat = muxer.generate_pat();
        assert_eq!(pat[0], SYNC_BYTE);
        assert_eq!(pat[5] & AF_DISCONTINUITY, AF_DISCONTINUITY);
    }

    #[test]
    fn ts_cc_increments_per_packet() {
        let pes = vec![0u8; 400];
        let mut cc = 0u8;
        let pkts = pes_to_ts_packets(&pes, VIDEO_PID, true, &mut cc, 0);
        assert!(pkts.len() >= TS_PACKET_SIZE * 2);
        assert_eq!(pkts[3] & 0x0F, 0);
        assert_eq!(pkts[TS_PACKET_SIZE + 3] & 0x0F, 1);
    }
}
