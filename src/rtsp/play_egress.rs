//! RTSP PLAY egress: jump to live edge and emit wall-clock RTP timestamps.

use std::time::{Duration, Instant};

use crate::core::{
    dispatch::DispatchReader,
    is_idr_frame, prime_live_play,
    CodecType, MediaFrame, StreamManager,
};

use super::common::{wrap_mpeg4_generic_aac_hbr, RtspCommon};

const RTP_CLOCK_HZ: u32 = 90_000;
const AAC_CLOCK_HZ: u32 = 44_100;

/// Wall-clock RTP timeline for live PLAY (avoids player buffering on publisher ts jumps).
pub struct PlayRtpTimeline {
    wall_anchor: Option<Instant>,
    clock_hz: u32,
}

impl Default for PlayRtpTimeline {
    fn default() -> Self {
        Self::for_codec(CodecType::H264)
    }
}

impl PlayRtpTimeline {
    pub fn for_codec(codec: CodecType) -> Self {
        Self {
            wall_anchor: None,
            clock_hz: match codec {
                CodecType::AAC => AAC_CLOCK_HZ,
                _ => RTP_CLOCK_HZ,
            },
        }
    }

    /// RTP timestamp from elapsed wall time since first packet (live play).
    pub fn map_wallclock(&mut self) -> u32 {
        let anchor = *self.wall_anchor.get_or_insert_with(Instant::now);
        let elapsed_us = anchor.elapsed().as_micros() as u64;
        let ts = match self.clock_hz {
            AAC_CLOCK_HZ => elapsed_us.saturating_mul(u64::from(AAC_CLOCK_HZ)) / 1_000_000,
            _ => elapsed_us.saturating_mul(90) / 1_000,
        };
        (ts & 0xFFFF_FFFF) as u32
    }
}

fn prepend_h264_config(manager: &StreamManager, stream_id: &str, frame: &MediaFrame) -> Vec<u8> {
    crate::core::live_play::prepend_h264_config(manager, stream_id, frame)
}

pub fn is_idr(frame: &MediaFrame) -> bool {
    is_idr_frame(frame)
}

/// Jump to live edge and wait for a fresh IDR (do not replay ring history).
pub async fn prime_rtsp_play(
    reader: &mut DispatchReader,
    manager: &StreamManager,
    stream_id: &str,
) -> Option<MediaFrame> {
    prime_live_play(reader, manager, stream_id, "RTSP").await
}

pub use crate::core::recv_coalesced_play_frame;

/// Build RTP packet(s) for one media frame using a local seq/timeline (never forward ingest RTP).
pub fn egress_rtp_packets(
    frame: &MediaFrame,
    manager: &StreamManager,
    stream_id: &str,
    timeline: &mut PlayRtpTimeline,
    seq: &mut u16,
    ssrc: u32,
) -> Vec<Vec<u8>> {
    let payload_type = rtp_payload_type_for_codec(manager, stream_id, frame.codec);
    let ts = timeline.map_wallclock();

    match frame.codec {
        CodecType::H264 => {
            let annex = prepend_h264_config(manager, stream_id, frame);
            RtspCommon::packetize_h264_access_unit_for_rtp(&annex, payload_type, seq, ts, ssrc)
        }
        CodecType::AAC => {
            if frame.data.len() < 4 {
                return Vec::new();
            }
            let payload = wrap_mpeg4_generic_aac_hbr(&frame.data);
            if payload.is_empty() {
                return Vec::new();
            }
            let pkt = RtspCommon::build_rtp_packet(payload_type, *seq, ts, ssrc, true, &payload);
            *seq = seq.wrapping_add(1);
            vec![pkt]
        }
        _ => Vec::new(),
    }
}

fn rtp_payload_type_for_codec(manager: &StreamManager, stream_id: &str, codec: CodecType) -> u8 {
    manager
        .get_stream(&stream_id.to_string())
        .and_then(|stream| {
            stream
                .tracks
                .iter()
                .find(|track| track.codec == codec)
                .map(|track| track.payload_type)
        })
        .unwrap_or(match codec {
            CodecType::H264 => 96,
            CodecType::AAC => 97,
            CodecType::Opus => 109,
            CodecType::H265 => 98,
            _ => 96,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{StreamProtocol, StreamSourceMode, Track};

    #[test]
    fn wallclock_timeline_monotonic() {
        let mut tl = PlayRtpTimeline::default();
        let t0 = tl.map_wallclock();
        std::thread::sleep(Duration::from_millis(5));
        let t1 = tl.map_wallclock();
        assert!(t1 >= t0);
    }

    #[test]
    fn rtp_payload_type_uses_stream_track_payload_type() {
        let manager = StreamManager::new();
        manager.create_stream(
            "webrtc_test",
            StreamSourceMode::Push,
            StreamProtocol::WebRTC,
            None,
        );
        manager.set_stream_tracks(
            "webrtc_test",
            vec![Track::new(0, CodecType::H264, 103, 90_000)],
        );

        assert_eq!(
            rtp_payload_type_for_codec(&manager, "webrtc_test", CodecType::H264),
            103
        );
    }
}
