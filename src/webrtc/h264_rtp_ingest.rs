//! Assemble H264 RTP into Annex B access units for StreamManager (RTSP / WebRTC ingest).

use bytes::Bytes;
use std::sync::Arc;
use tracing::{debug, info, warn};
use webrtc::rtp::codecs::h264::H264Packet;
use webrtc::rtp::packet::Packet;
use webrtc::rtp::packetizer::Depacketizer;

use crate::core::{CodecType, MediaFrame, StreamManager, VIDEO_RTP_CLOCK_RATE};

use super::h264_util::{
    contains_vcl_nalu, describe_annex_b, is_keyframe_annex_b, is_parameter_set_only,
};
use super::publish_signaling::latest_keyframe_request_age_ms;
use super::rtp_h264::{
    self, annex_b_from_rtp_payload, describe_rtp_payload, extract_sps_pps_from_nalus,
    is_idr_rtp_payload, parse_rtp_h264,
};

/// Stateful H264 RTP → Annex B access-unit publisher.
pub struct H264RtpIngest {
    stream_id: String,
    manager: Arc<StreamManager>,
    depacketizer: H264Packet,
    batch: AccessUnitBatch,
    expected_seq: Option<u16>,
    units: u64,
    label: &'static str,
}

#[derive(Default)]
struct AccessUnitBatch {
    timestamp: u32,
    parts: Vec<Bytes>,
    is_keyframe: bool,
}

impl H264RtpIngest {
    pub fn new(manager: Arc<StreamManager>, stream_id: String, label: &'static str) -> Self {
        Self {
            stream_id,
            manager,
            depacketizer: H264Packet::default(),
            batch: AccessUnitBatch::default(),
            expected_seq: None,
            units: 0,
            label,
        }
    }

    /// Parse a full RTP packet (header + payload), ingest H264, publish on marker.
    pub fn ingest_rtp_packet(&mut self, rtp: &[u8]) -> bool {
        let Some(info) = rtp_h264_packet_info(rtp) else {
            return false;
        };
        if !self.accept_sequence(info.sequence_number, info.payload) {
            return false;
        }
        self.ingest_payload(info.payload, info.timestamp, info.marker)
    }

    /// Ingest H264 RTP payload with timestamp and marker bit.
    pub fn ingest_payload(&mut self, payload: &[u8], timestamp: u32, marker: bool) -> bool {
        if payload.is_empty() {
            return false;
        }

        let pkt = Packet {
            header: webrtc::rtp::header::Header {
                marker,
                timestamp,
                ..Default::default()
            },
            payload: Bytes::copy_from_slice(payload),
        };

        let mut published = false;
        if let Some(frame) =
            h264_rtp_to_annex_b(&mut self.depacketizer, &pkt, &self.stream_id, &self.manager)
        {
            if is_parameter_set_only(&frame.data) {
                return false;
            }
            if !self.batch.parts.is_empty() && self.batch.timestamp != timestamp {
                if self.flush_access_unit() {
                    published = true;
                }
            }
            if self.batch.parts.is_empty() {
                self.batch.timestamp = timestamp;
            }
            self.batch.parts.push(frame.data);
            self.batch.is_keyframe |= frame.is_keyframe;
            if marker && self.flush_access_unit() {
                published = true;
            }
        }
        published
    }

    pub fn flush_remaining(&mut self) -> bool {
        self.flush_access_unit()
    }

    fn flush_access_unit(&mut self) -> bool {
        if self.batch.parts.is_empty() {
            return false;
        }

        let mut combined = Vec::new();
        for part in &self.batch.parts {
            combined.extend_from_slice(part);
        }
        let is_keyframe = self.batch.is_keyframe || is_keyframe_annex_b(&combined);
        let desc = describe_annex_b(&combined);
        let keyframe_request = if is_keyframe {
            latest_keyframe_request_age_ms(&self.stream_id)
        } else {
            None
        };
        if !contains_vcl_nalu(&combined) {
            self.batch.parts.clear();
            self.batch.is_keyframe = false;
            return false;
        }
        self.units += 1;
        let n = self.units;
        let size = combined.len();

        if n == 1 {
            info!(
                "[{}] First access unit stream='{}' size={} keyframe={} ts={} [{}]",
                self.label, self.stream_id, size, is_keyframe, self.batch.timestamp, desc
            );
        } else if n <= 5 || is_keyframe || n % 100 == 0 {
            info!(
                "[{}] Access unit #{} stream='{}' keyframe={} ts={} [{}]",
                self.label, n, self.stream_id, is_keyframe, self.batch.timestamp, desc
            );
        } else {
            debug!(
                "[{}] Access unit #{} stream='{}' ts={}",
                self.label, n, self.stream_id, self.batch.timestamp
            );
        }

        let frame = MediaFrame::new(
            self.stream_id.clone(),
            0,
            self.batch.timestamp as u64,
            Bytes::from(combined),
            is_keyframe,
            CodecType::H264,
        )
        .with_clock_rate(VIDEO_RTP_CLOCK_RATE);
        self.manager.publish_frame(frame);
        if is_keyframe {
            let ring_seq = self
                .manager
                .get_hub(&self.stream_id)
                .map(|hub| hub.latest_seq());
            match keyframe_request {
                Some((request_id, age_ms)) => info!(
                    "[{}] Published keyframe response stream='{}' request_id={} request_age_ms={} ring_seq={:?} rtp_ts={} size={}",
                    self.label,
                    self.stream_id,
                    request_id,
                    age_ms,
                    ring_seq,
                    self.batch.timestamp,
                    size
                ),
                None => info!(
                    "[{}] Published keyframe stream='{}' request_id=none ring_seq={:?} rtp_ts={} size={}",
                    self.label,
                    self.stream_id,
                    ring_seq,
                    self.batch.timestamp,
                    size
                ),
            }
        }

        self.batch.parts.clear();
        self.batch.is_keyframe = false;
        true
    }

    fn accept_sequence(&mut self, seq: u16, payload: &[u8]) -> bool {
        let expected_seq = self.expected_seq;
        let gap = expected_seq
            .map(|expected| seq != expected)
            .unwrap_or(false);
        self.expected_seq = Some(seq.wrapping_add(1));

        if !gap {
            return true;
        }

        warn!(
            "[{}] RTP sequence gap stream='{}' expected={:?} got={} payload={}",
            self.label,
            self.stream_id,
            expected_seq,
            seq,
            describe_rtp_payload(payload)
        );
        self.discard_partial_access_unit();

        // A continuation/end FU-A after a gap cannot be reconstructed safely.
        !is_fu_a_continuation(payload)
    }

    fn discard_partial_access_unit(&mut self) {
        self.depacketizer = H264Packet::default();
        self.batch.parts.clear();
        self.batch.is_keyframe = false;
    }
}

struct RtpH264PacketInfo<'a> {
    payload: &'a [u8],
    timestamp: u32,
    marker: bool,
    sequence_number: u16,
}

/// Strip RTP header (and CSRC / extensions) → (payload, timestamp, marker).
pub fn rtp_h264_media_payload(rtp: &[u8]) -> Option<(&[u8], u32, bool)> {
    let info = rtp_h264_packet_info(rtp)?;
    Some((info.payload, info.timestamp, info.marker))
}

fn rtp_h264_packet_info(rtp: &[u8]) -> Option<RtpH264PacketInfo<'_>> {
    if rtp.len() < 12 {
        return None;
    }
    let marker = (rtp[1] & 0x80) != 0;
    let sequence_number = u16::from_be_bytes(rtp[2..4].try_into().ok()?);
    let timestamp = u32::from_be_bytes(rtp[4..8].try_into().ok()?);
    let extension = (rtp[0] >> 4) & 0x01;
    let csrc_count = (rtp[0] & 0x0F) as usize;
    let mut offset = 12 + csrc_count * 4;
    if offset > rtp.len() {
        return None;
    }
    if extension != 0 {
        if offset + 4 > rtp.len() {
            return None;
        }
        let ext_len = ((rtp[offset + 2] as usize) << 8 | rtp[offset + 3] as usize) * 4 + 4;
        offset += ext_len;
    }
    if offset >= rtp.len() {
        return None;
    }
    Some(RtpH264PacketInfo {
        payload: &rtp[offset..],
        timestamp,
        marker,
        sequence_number,
    })
}

fn is_fu_a_continuation(payload: &[u8]) -> bool {
    if payload.len() < 2 || (payload[0] & 0x1F) != 28 {
        return false;
    }
    let start = (payload[1] & 0x80) != 0;
    !start
}

fn store_nalu_config_from_rtp(manager: &StreamManager, stream_id: &str, payload: &[u8]) {
    let nalus = parse_rtp_h264(payload);
    let (sps, pps) = extract_sps_pps_from_nalus(&nalus);
    if let (Some(sps), Some(pps)) = (sps, pps) {
        info!(
            "[H264] Stored SPS/PPS stream='{}' sps={} pps={} [{}]",
            stream_id,
            sps.len(),
            pps.len(),
            describe_rtp_payload(payload)
        );
        manager.set_stream_sps_pps(stream_id, sps, pps);
        return;
    }
    if manager
        .get_stream(&stream_id.to_string())
        .map(|s| s.sps.is_some() && s.pps.is_some())
        .unwrap_or(false)
    {
        return;
    }
    for n in &nalus {
        if n.nal_type == 7 || n.nal_type == 8 {
            manager.merge_stream_nalu_config(stream_id, &n.data);
        }
    }
}

fn h264_rtp_to_annex_b(
    depacketizer: &mut H264Packet,
    pkt: &Packet,
    stream_id: &str,
    manager: &StreamManager,
) -> Option<MediaFrame> {
    let payload = &pkt.payload;
    store_nalu_config_from_rtp(manager, stream_id, payload);

    let rtp_nalus = parse_rtp_h264(payload);
    let is_keyframe_rtp = rtp_h264::contains_idr(&rtp_nalus) || is_idr_rtp_payload(payload);

    let depayload = Bytes::copy_from_slice(payload);
    let annex_b = match depacketizer.depacketize(&depayload) {
        Ok(nalu) if !nalu.is_empty() => nalu,
        Ok(_) => return None,
        Err(_) => {
            return annex_b_from_rtp_payload(payload).map(|annex_b| {
                let is_keyframe = is_keyframe_rtp || is_keyframe_annex_b(&annex_b);
                MediaFrame::new(
                    stream_id.to_string(),
                    0,
                    pkt.header.timestamp as u64,
                    annex_b,
                    is_keyframe,
                    CodecType::H264,
                )
                .with_clock_rate(VIDEO_RTP_CLOCK_RATE)
            });
        }
    };

    let is_keyframe = is_keyframe_rtp || is_keyframe_annex_b(&annex_b);
    Some(
        MediaFrame::new(
            stream_id.to_string(),
            0,
            pkt.header.timestamp as u64,
            annex_b,
            is_keyframe,
            CodecType::H264,
        )
        .with_clock_rate(VIDEO_RTP_CLOCK_RATE),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{StreamManager, StreamProtocol, StreamSourceMode};

    fn rtp_packet(seq: u16, timestamp: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + payload.len());
        out.push(0x80);
        out.push(if marker { 0x80 | 96 } else { 96 });
        out.extend_from_slice(&seq.to_be_bytes());
        out.extend_from_slice(&timestamp.to_be_bytes());
        out.extend_from_slice(&0x1122_3344u32.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn manager_with_stream() -> Arc<StreamManager> {
        let manager = Arc::new(StreamManager::new());
        manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::RTSP, None);
        manager
    }

    fn fu_a_start_idr(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x7c, 0x85];
        out.extend_from_slice(data);
        out
    }

    fn fu_a_mid_idr(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x7c, 0x05];
        out.extend_from_slice(data);
        out
    }

    fn fu_a_end_idr(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x7c, 0x45];
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn sequence_gap_drops_incomplete_fu_a_access_unit_and_recovers() {
        let manager = manager_with_stream();
        let mut ingest = H264RtpIngest::new(manager.clone(), "s".to_string(), "test");

        assert!(!ingest.ingest_rtp_packet(&rtp_packet(
            1,
            90_000,
            false,
            &fu_a_start_idr(&[0x88, 0x99])
        )));
        assert!(!ingest.ingest_rtp_packet(&rtp_packet(
            3,
            90_000,
            true,
            &fu_a_end_idr(&[0xaa, 0xbb])
        )));
        assert!(manager.get_hub("s").expect("stream hub").is_empty());

        assert!(ingest.ingest_rtp_packet(&rtp_packet(4, 93_600, true, &[0x65, 0x88, 0x84])));
        let hub = manager
            .get_hub("s")
            .expect("complete packet should publish");
        let frame = hub.get(0).expect("published frame");
        assert!(frame.is_keyframe);
        assert_eq!(frame.timestamp, 93_600);
    }

    #[test]
    fn complete_fu_a_access_unit_publishes_once() {
        let manager = manager_with_stream();
        let mut ingest = H264RtpIngest::new(manager.clone(), "s".to_string(), "test");

        assert!(!ingest.ingest_rtp_packet(&rtp_packet(
            10,
            90_000,
            false,
            &fu_a_start_idr(&[0x88])
        )));
        assert!(!ingest.ingest_rtp_packet(&rtp_packet(11, 90_000, false, &fu_a_mid_idr(&[0x99]))));
        assert!(ingest.ingest_rtp_packet(&rtp_packet(12, 90_000, true, &fu_a_end_idr(&[0xaa]))));

        let hub = manager.get_hub("s").expect("complete FU-A should publish");
        assert_eq!(hub.latest_seq(), 0);
        let frame = hub.get(0).expect("published frame");
        assert!(frame.is_keyframe);
        assert_eq!(frame.timestamp, 90_000);
    }
}
