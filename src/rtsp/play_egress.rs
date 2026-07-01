//! RTSP PLAY egress: jump to live edge and emit wall-clock RTP timestamps.

use std::time::{Duration, Instant};

use tracing::info;

use crate::core::{
    dispatch::{DispatchError, DispatchReader},
    CodecType, MediaFrame, StreamManager,
};
use crate::webrtc::h264_util::{
    contains_idr_nalu, contains_sps_or_pps_nalu, ensure_annex_b, is_parameter_set_only,
};
use crate::webrtc::annex_b_with_config;

use super::common::{RtspCommon, wrap_mpeg4_generic_aac_hbr};

const RTP_CLOCK_HZ: u32 = 90_000;
const AAC_CLOCK_HZ: u32 = 44_100;
const LIVE_IDR_WAIT: Duration = Duration::from_millis(2500);

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

pub fn is_idr(frame: &MediaFrame) -> bool {
    frame.is_keyframe || contains_idr_nalu(&ensure_annex_b(&frame.data))
}

fn prepare_h264_play_frame(
    manager: &StreamManager,
    stream_id: &str,
    frame: MediaFrame,
) -> MediaFrame {
    let annex = prepend_h264_config(manager, stream_id, &frame);
    MediaFrame::new(
        stream_id.to_string(),
        frame.track_id,
        frame.timestamp,
        bytes::Bytes::from(annex),
        true,
        frame.codec,
    )
}

/// Jump to live edge and wait for a fresh IDR (do not replay ring history).
pub async fn prime_rtsp_play(
    reader: &mut DispatchReader,
    manager: &StreamManager,
    stream_id: &str,
) -> Option<MediaFrame> {
    reader.finish_prime();

    let deadline = Instant::now() + LIVE_IDR_WAIT;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, reader.recv_coalesced()).await {
            Ok(Ok(frame)) if is_playable_video(&frame) && is_idr(&frame) => {
                info!(
                    "[RTSP] [{}] PLAY live IDR ts={} bytes={}",
                    stream_id,
                    frame.timestamp,
                    frame.data.len()
                );
                return Some(prepare_h264_play_frame(manager, stream_id, frame));
            }
            Ok(Ok(_)) => continue,
            Ok(Err(DispatchError::Closed)) => break,
            Err(_) => break,
        }
    }

    if let Some(idr) = reader.hub().latest_idr_frame() {
        if is_playable_video(&idr) && is_idr(&idr) {
            info!(
                "[RTSP] [{}] PLAY fallback IDR ts={} bytes={}",
                stream_id,
                idr.timestamp,
                idr.data.len()
            );
            reader.finish_prime();
            return Some(prepare_h264_play_frame(manager, stream_id, idr));
        }
    }

    info!("[RTSP] [{}] PLAY no IDR yet — wait in relay loop", stream_id);
    None
}

/// Block for one frame, coalescing video bursts to the latest playable frame.
pub async fn recv_coalesced_play_frame(
    reader: &mut DispatchReader,
) -> Result<MediaFrame, DispatchError> {
    loop {
        let frame = reader.recv_coalesced().await?;
        if is_playable_video(&frame) || !matches!(frame.codec, CodecType::H264 | CodecType::H265) {
            return Ok(frame);
        }
    }
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
    let ts = timeline.map_wallclock();

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
    fn wallclock_timeline_monotonic() {
        let mut tl = PlayRtpTimeline::default();
        let t0 = tl.map_wallclock();
        std::thread::sleep(Duration::from_millis(5));
        let t1 = tl.map_wallclock();
        assert!(t1 >= t0);
    }
}
