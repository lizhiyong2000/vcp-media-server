//! RTSP PLAY egress: jump to live edge and emit monotonic RTP timestamps/sequence.

use std::time::Instant;

use tokio::sync::broadcast;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tracing::info;

use crate::core::{CodecType, MediaFrame, StreamManager};
use crate::webrtc::request_publisher_keyframe;
use crate::webrtc::h264_util::{contains_idr_nalu, contains_sps_or_pps_nalu, ensure_annex_b, is_parameter_set_only};
use crate::webrtc::annex_b_with_config;

use super::common::{RtspCommon, wrap_mpeg4_generic_aac_hbr};

const RTP_CLOCK_HZ: u32 = 90_000;
const AAC_CLOCK_HZ: u32 = 44_100;
/// Max RTP timestamp advance per frame (~160 ms @ 90 kHz).
const MAX_FRAME_DELTA: u32 = 14_400;

/// Map publisher timestamps to a monotonic RTP timeline for this PLAY session.
pub struct PlayRtpTimeline {
    wall_anchor: Option<Instant>,
    last_src: Option<u64>,
    out_ts: u32,
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
            last_src: None,
            out_ts: 0,
            clock_hz: match codec {
                CodecType::AAC => AAC_CLOCK_HZ,
                _ => RTP_CLOCK_HZ,
            },
        }
    }

    fn min_frame_ticks(&self) -> u32 {
        if self.clock_hz == AAC_CLOCK_HZ {
            1024
        } else {
            3_000
        }
    }

    /// Anchor wall clock at first egress frame (after prime), not at PLAY request.
    pub fn map(&mut self, src_ts: u64, is_keyframe: bool) -> u32 {
        if self.wall_anchor.is_none() {
            self.wall_anchor = Some(Instant::now());
            self.last_src = Some(src_ts);
            self.out_ts = 0;
            return 0;
        }

        let min_step = self.min_frame_ticks();
        let last = self.last_src.unwrap();
        if src_ts > last {
            let gap = src_ts - last;
            // Cached keyframe → live edge: do not map the full publisher gap into RTP ts.
            if gap > u64::from(self.clock_hz) / 4 {
                self.wall_anchor = Some(Instant::now());
                self.last_src = Some(src_ts);
                self.out_ts = self.out_ts.wrapping_add(min_step);
                return self.out_ts;
            }
            let raw = gap.min(u64::from(self.clock_hz)) as u32;
            let delta = if self.clock_hz == RTP_CLOCK_HZ && raw > 2000 {
                raw
            } else if self.clock_hz == AAC_CLOCK_HZ {
                raw.max(min_step)
            } else {
                raw.saturating_mul(90).max(min_step)
            };
            self.out_ts = self
                .out_ts
                .wrapping_add(delta.max(min_step).min(MAX_FRAME_DELTA));
            self.last_src = Some(src_ts);
        } else if is_keyframe {
            self.out_ts = self.out_ts.wrapping_add(min_step);
            self.last_src = Some(src_ts);
        } else {
            self.out_ts = self.out_ts.wrapping_add(min_step);
            self.last_src = Some(src_ts);
        }
        self.out_ts
    }
}

fn prepend_h264_config(manager: &StreamManager, stream_id: &str, frame: &MediaFrame) -> Vec<u8> {
    let au = ensure_annex_b(&frame.data);
    if !(frame.is_keyframe || is_idr(frame) || contains_sps_or_pps_nalu(&au)) {
        return au;
    }
    if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
        if let (Some(sps), Some(pps)) = (&stream.sps, &stream.pps) {
            return annex_b_with_config(sps, pps, &au);
        }
    }
    au
}

fn is_playable_video(frame: &MediaFrame) -> bool {
    if !matches!(frame.codec, CodecType::H264 | CodecType::H265) {
        return false;
    }
    let data = ensure_annex_b(&frame.data);
    !data.is_empty() && !is_parameter_set_only(&data)
}

fn is_idr(frame: &MediaFrame) -> bool {
    frame.is_keyframe || contains_idr_nalu(&ensure_annex_b(&frame.data))
}

/// Drop queued broadcast frames; optionally keep SPS/PPS on the stream.
pub fn flush_stale_rx(
    rx: &mut broadcast::Receiver<MediaFrame>,
    manager: &StreamManager,
    stream_id: &str,
) -> u64 {
    let mut dropped = 0u64;
    loop {
        match rx.try_recv() {
            Ok(frame) => {
                dropped += 1;
                if matches!(frame.codec, CodecType::H264 | CodecType::H265) {
                    let data = ensure_annex_b(&frame.data);
                    for (s, e) in crate::webrtc::h264_util::iter_annex_b_nal_ranges(&data) {
                        manager.merge_stream_nalu_config(stream_id, &data[s..e]);
                    }
                }
            }
            Err(TryRecvError::Lagged(n)) => dropped += n,
            Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
        }
    }
    dropped
}

/// After flush, wait briefly for a live IDR; fall back to cached keyframe with SPS/PPS.
pub async fn prime_rtsp_play_rx(
    rx: &mut broadcast::Receiver<MediaFrame>,
    manager: &StreamManager,
    stream_id: &str,
) -> Option<MediaFrame> {
    request_publisher_keyframe(stream_id);
    let dropped = flush_stale_rx(rx, manager, stream_id);
    if dropped > 0 {
        info!(
            "[RTSP] [{}] PLAY flushed {} stale frames before live edge",
            stream_id, dropped
        );
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(800);
    let mut pending: Option<MediaFrame> = None;

    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(frame)) => {
                if is_playable_video(&frame) && is_idr(&frame) {
                    info!(
                        "[RTSP] [{}] PLAY primed live IDR ts={}",
                        stream_id, frame.timestamp
                    );
                    return Some(frame);
                }
                if is_playable_video(&frame) {
                    pending = Some(frame);
                }
            }
            Ok(Err(RecvError::Lagged(n))) => {
                info!("[RTSP] [{}] PLAY prime lagged {} frames", stream_id, n);
                flush_stale_rx(rx, manager, stream_id);
                pending = None;
            }
            Ok(Err(RecvError::Closed)) => return None,
            Err(_) => break,
        }
    }

    if let Some((data, ts)) = manager.get_last_keyframe(stream_id) {
        let annex = prepend_h264_config(
            manager,
            stream_id,
            &MediaFrame::new(
                stream_id.to_string(),
                0,
                ts,
                bytes::Bytes::from(data.clone()),
                true,
                CodecType::H264,
            ),
        );
        info!(
            "[RTSP] [{}] PLAY primed from cached keyframe ts={} bytes={}",
            stream_id,
            ts,
            annex.len()
        );
        return Some(MediaFrame::new(
            stream_id.to_string(),
            0,
            ts,
            bytes::Bytes::from(annex),
            true,
            CodecType::H264,
        ));
    }

    None
}

/// Block for one frame, then coalesce any burst to the latest playable frame.
pub async fn recv_coalesced_play_frame(
    rx: &mut broadcast::Receiver<MediaFrame>,
) -> Result<MediaFrame, RecvError> {
    let mut latest = rx.recv().await?;
    loop {
        match rx.try_recv() {
            Ok(next) => {
                if is_playable_video(&next) {
                    latest = next;
                }
            }
            Err(TryRecvError::Lagged(n)) => return Err(RecvError::Lagged(n)),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Closed) => return Err(RecvError::Closed),
        }
    }
    Ok(latest)
}

/// Build RTP packet(s) for one media frame using a local seq/timeline (never forward ingest RTP).
pub fn egress_rtp_packets(
    frame: &MediaFrame,
    manager: &StreamManager,
    stream_id: &str,
    timeline: &mut PlayRtpTimeline,
    seq: &mut u16,
    ssrc: u32,
) -> Vec<Vec<u8>> {
    let payload_type = match frame.codec {
        CodecType::H264 => 96,
        CodecType::AAC => 97,
        _ => 96,
    };
    let ts = timeline.map(frame.timestamp, frame.is_keyframe);

    match frame.codec {
        CodecType::H264 => {
            let annex = prepend_h264_config(manager, stream_id, frame);
            RtspCommon::packetize_h264_access_unit_for_rtp(
                &annex,
                payload_type,
                seq,
                ts,
                ssrc,
            )
        }
        CodecType::AAC => {
            if frame.data.len() < 4 {
                return Vec::new();
            }
            let payload = wrap_mpeg4_generic_aac_hbr(&frame.data);
            if payload.is_empty() {
                return Vec::new();
            }
            let pkt = RtspCommon::build_rtp_packet(
                payload_type,
                *seq,
                ts,
                ssrc,
                true,
                &payload,
            );
            *seq = seq.wrapping_add(1);
            vec![pkt]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_always_advances_on_flat_src_ts() {
        let mut tl = PlayRtpTimeline::default();
        let t0 = tl.map(1_000_000, true);
        let t1 = tl.map(1_000_000, false);
        let t2 = tl.map(999_000, false);
        assert_eq!(t0, 0);
        assert!(t1 > t0);
        assert!(t2 > t1);
    }

    #[test]
    fn timeline_uses_90khz_src_delta() {
        let mut tl = PlayRtpTimeline::default();
        let t0 = tl.map(1_000_000, true);
        let t1 = tl.map(1_003_600, false);
        assert_eq!(t0, 0);
        assert_eq!(t1, 3600);
    }

    #[test]
    fn timeline_caps_large_publisher_gap() {
        let mut tl = PlayRtpTimeline::default();
        let t0 = tl.map(1_000_000, true);
        let t1 = tl.map(1_064_800, false);
        assert_eq!(t0, 0);
        assert!(t1 <= 14_400);
    }
}
