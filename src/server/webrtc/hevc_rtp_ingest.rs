//! Assemble HEVC RTP into Annex B access units for StreamManager (RTSP ingest).

use bytes::Bytes;
use std::sync::Arc;
use tracing::debug;

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

        let mut published = false;
        if !self.batch.parts.is_empty() && self.batch.timestamp != timestamp {
            published = self.flush_access_unit();
        }
        if self.batch.parts.is_empty() {
            self.batch.timestamp = timestamp;
        }
        self.merge_hevc_config_from_annex_b(&annex_b);
        self.batch.parts.push(annex_b);
        self.batch.is_keyframe |= is_key;

        if marker {
            return self.flush_access_unit() || published;
        }
        published
    }

    fn flush_access_unit(&mut self) -> bool {
        if self.batch.parts.is_empty() {
            return false;
        }

        let mut combined = Vec::new();
        for part in &self.batch.parts {
            combined.extend_from_slice(part);
        }
        self.set_complete_hevc_config_from_access_unit(&combined);
        let is_keyframe = self.batch.is_keyframe || is_keyframe_annex_b(CodecType::H265, &combined);
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

    fn merge_hevc_config_from_annex_b(&self, annex_b: &[u8]) {
        let Some(nal) = annex_b_nalu_payload(annex_b) else {
            return;
        };
        self.manager
            .merge_stream_nalu_config(&self.stream_id, CodecType::H265, nal);
    }

    fn set_complete_hevc_config_from_access_unit(&self, access_unit: &[u8]) {
        let (vps, sps, pps) = extract_hevc_param_sets(access_unit);
        if let (Some(vps), Some(sps), Some(pps)) = (vps, sps, pps) {
            self.manager
                .set_stream_hevc_params(&self.stream_id, vps, sps, pps);
        }
    }
}

fn annex_b_nalu_payload(data: &[u8]) -> Option<&[u8]> {
    if data.starts_with(&[0, 0, 0, 1]) {
        return Some(&data[4..]);
    }
    if data.starts_with(&[0, 0, 1]) {
        return Some(&data[3..]);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{StreamProtocol, StreamSourceMode};

    #[test]
    fn mixed_hevc_access_unit_updates_complete_parameter_sets() {
        let manager = Arc::new(StreamManager::new());
        manager.create_stream(
            "hevc_rtsp",
            StreamSourceMode::Push,
            StreamProtocol::RTSP,
            None,
        );
        let ingest = HevcRtpIngest::new(manager.clone(), "hevc_rtsp".into(), "test");
        let vps = [0x40, 0x01, 0x0c, 0x01];
        let sps = [0x42, 0x01, 0x01, 0x60];
        let pps = [0x44, 0x01, 0xc0, 0x73];
        let cra = [0x2a, 0x01, 0x80, 0x00];
        let mut au = Vec::new();
        for nal in [&vps[..], &sps[..], &pps[..], &cra[..]] {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(nal);
        }

        ingest.set_complete_hevc_config_from_access_unit(&au);

        let stream = manager.get_stream(&"hevc_rtsp".to_string()).unwrap();
        assert_eq!(stream.vps.as_deref(), Some(vps.as_slice()));
        assert_eq!(stream.sps.as_deref(), Some(sps.as_slice()));
        assert_eq!(stream.pps.as_deref(), Some(pps.as_slice()));
    }

    #[test]
    fn parameter_sets_share_timestamp_with_cra_in_published_access_unit() {
        let manager = Arc::new(StreamManager::new());
        manager.create_stream(
            "hevc_rtsp",
            StreamSourceMode::Push,
            StreamProtocol::RTSP,
            None,
        );
        let mut ingest = HevcRtpIngest::new(manager.clone(), "hevc_rtsp".into(), "test");
        let vps = [0x40, 0x01, 0x0c, 0x01];
        let sps = [0x42, 0x01, 0x01, 0x60];
        let pps = [0x44, 0x01, 0xc0, 0x73];
        let cra = [0x2a, 0x01, 0x80, 0x00];

        assert!(!ingest.ingest_payload(&vps, 90_000, false));
        assert!(!ingest.ingest_payload(&sps, 90_000, false));
        assert!(!ingest.ingest_payload(&pps, 90_000, false));
        assert!(ingest.ingest_payload(&cra, 90_000, true));

        let hub = manager.get_hub("hevc_rtsp").unwrap();
        let frame = hub.get(hub.latest_seq()).unwrap();
        let (parsed_vps, parsed_sps, parsed_pps) = extract_hevc_param_sets(&frame.data);
        assert!(frame.is_keyframe);
        assert_eq!(frame.track_id, 0);
        assert_eq!(parsed_vps.as_deref(), Some(vps.as_slice()));
        assert_eq!(parsed_sps.as_deref(), Some(sps.as_slice()));
        assert_eq!(parsed_pps.as_deref(), Some(pps.as_slice()));
    }

    #[test]
    fn timestamp_rollover_flush_does_not_drop_current_payload() {
        let manager = Arc::new(StreamManager::new());
        manager.create_stream(
            "hevc_rtsp",
            StreamSourceMode::Push,
            StreamProtocol::RTSP,
            None,
        );
        let mut ingest = HevcRtpIngest::new(manager.clone(), "hevc_rtsp".into(), "test");
        let cra = [0x2a, 0x01, 0x80, 0x00];
        let trail = [0x02, 0x01, 0x11, 0x22];

        assert!(!ingest.ingest_payload(&cra, 90_000, false));
        assert!(ingest.ingest_payload(&trail, 93_600, true));

        let hub = manager.get_hub("hevc_rtsp").unwrap();
        assert_eq!(hub.latest_seq(), 1);
        assert_eq!(hub.get(0).unwrap().timestamp, 90_000);
        assert_eq!(hub.get(1).unwrap().timestamp, 93_600);
    }
}
