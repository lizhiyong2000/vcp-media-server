//! Assemble H264 RTP into Annex B access units for StreamManager (RTSP / WebRTC ingest).

use bytes::Bytes;
use std::sync::Arc;
use tracing::{debug, info};
use webrtc::rtp::codecs::h264::H264Packet;
use webrtc::rtp::packet::Packet;
use webrtc::rtp::packetizer::Depacketizer;

use crate::core::{CodecType, MediaFrame, StreamManager};

use super::h264_util::{
    contains_vcl_nalu, describe_annex_b, is_keyframe_annex_b, is_parameter_set_only,
};
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
            units: 0,
            label,
        }
    }

    /// Parse a full RTP packet (header + payload), ingest H264, publish on marker.
    pub fn ingest_rtp_packet(&mut self, rtp: &[u8]) -> bool {
        let Some((payload, ts, marker)) = rtp_h264_media_payload(rtp) else {
            return false;
        };
        self.ingest_payload(payload, ts, marker)
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
        if !contains_vcl_nalu(&combined) {
            self.batch.parts.clear();
            self.batch.is_keyframe = false;
            return false;
        }
        self.units += 1;
        let n = self.units;

        if n == 1 {
            info!(
                "[{}] First access unit stream='{}' size={} keyframe={} ts={} [{}]",
                self.label,
                self.stream_id,
                combined.len(),
                is_keyframe,
                self.batch.timestamp,
                desc
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
        );
        self.manager.publish_frame(frame);

        self.batch.parts.clear();
        self.batch.is_keyframe = false;
        true
    }
}

/// Strip RTP header (and CSRC / extensions) → (payload, timestamp, marker).
pub fn rtp_h264_media_payload(rtp: &[u8]) -> Option<(&[u8], u32, bool)> {
    if rtp.len() < 12 {
        return None;
    }
    let marker = (rtp[1] & 0x80) != 0;
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
    Some((&rtp[offset..], timestamp, marker))
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
            });
        }
    };

    let is_keyframe = is_keyframe_rtp || is_keyframe_annex_b(&annex_b);
    Some(MediaFrame::new(
        stream_id.to_string(),
        0,
        pkt.header.timestamp as u64,
        annex_b,
        is_keyframe,
        CodecType::H264,
    ))
}
