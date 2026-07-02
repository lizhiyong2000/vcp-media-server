//! Assemble HEVC RTP into Annex B access units for StreamManager (RTSP ingest).

use bytes::Bytes;
use std::sync::Arc;
use tracing::{debug, info};

use crate::core::{
    extract_hevc_param_sets, is_keyframe_annex_b, is_parameter_set_only, CodecType,
    HevcFuReassembler, MediaFrame, StreamManager, VIDEO_RTP_CLOCK_RATE,
};

use super::h264_rtp_ingest::rtp_h264_media_payload;

pub struct HevcRtpIngest {
    stream_id: String,
    manager: Arc<StreamManager>,
    fu: HevcFuReassembler,
    batch: AccessUnitBatch,
    label: &'static str,
}

#[derive(Default)]
struct AccessUnitBatch {
    timestamp: u32,
    parts: Vec<Bytes>,
    is_keyframe: bool,
}

impl HevcRtpIngest {
    pub fn new(manager: Arc<StreamManager>, stream_id: String, label: &'static str) -> Self {
        Self {
            stream_id,
            manager,
            fu: HevcFuReassembler::default(),
            batch: AccessUnitBatch::default(),
            label,
        }
    }

    pub fn ingest_rtp_packet(&mut self, rtp: &[u8]) -> bool {
        let Some((payload, timestamp, marker)) = rtp_h264_media_payload(rtp) else {
            return false;
        };
        self.ingest_payload(payload, timestamp, marker)
    }

    pub fn ingest_payload(&mut self, payload: &[u8], timestamp: u32, marker: bool) -> bool {
        if payload.is_empty() {
            return false;
        }

        let Some((annex_b, is_key)) = self.fu.depacketize(payload) else {
            if marker {
                return self.flush_access_unit();
            }
            return false;
        };

        if is_parameter_set_only(CodecType::H265, &annex_b) {
            let (vps, sps, pps) = extract_hevc_param_sets(&annex_b);
            if sps.is_some() {
                self.manager.set_stream_hevc_params(
                    &self.stream_id,
                    vps.unwrap_or_default(),
                    sps.unwrap_or_default(),
                    pps.unwrap_or_default(),
                );
            }
            return false;
        }

        if !self.batch.parts.is_empty() && self.batch.timestamp != timestamp {
            if self.flush_access_unit() {
                return true;
            }
        }
        if self.batch.parts.is_empty() {
            self.batch.timestamp = timestamp;
        }
        self.batch.parts.push(annex_b);
        self.batch.is_keyframe |= is_key;

        if marker {
            return self.flush_access_unit();
        }
        false
    }

    fn flush_access_unit(&mut self) -> bool {
        if self.batch.parts.is_empty() {
            return false;
        }

        let mut combined = Vec::new();
        for part in &self.batch.parts {
            combined.extend_from_slice(part);
        }
        let is_keyframe =
            self.batch.is_keyframe || is_keyframe_annex_b(CodecType::H265, &combined);
        if is_parameter_set_only(CodecType::H265, &combined) {
            self.batch.parts.clear();
            self.batch.is_keyframe = false;
            return false;
        }

        let frame = MediaFrame::new(
            self.stream_id.clone(),
            0,
            self.batch.timestamp as u64,
            Bytes::from(combined),
            is_keyframe,
            CodecType::H265,
        )
        .with_clock_rate(VIDEO_RTP_CLOCK_RATE);
        self.manager.publish_frame(frame);
        debug!(
            "[{}] HEVC access unit stream='{}' keyframe={} ts={}",
            self.label, self.stream_id, is_keyframe, self.batch.timestamp
        );

        self.batch.parts.clear();
        self.batch.is_keyframe = false;
        true
    }
}
