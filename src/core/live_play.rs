//! Low-latency live play: snap to live edge, wait for fresh IDR, coalesce bursts.

use std::time::{Duration, Instant};

use tracing::info;

use super::dispatch::{last_playable_idr_seq, DispatchError, DispatchReader};
use super::{CodecType, MediaFrame, StreamManager};
use crate::core::{
    is_keyframe_annex_b, is_parameter_set_only as param_only, prepend_annex_b_config,
};
use crate::server::webrtc::h264_util::{contains_sps_or_pps_nalu, ensure_annex_b};
use crate::server::webrtc::request_publisher_keyframe;

const LIVE_IDR_WAIT: Duration = Duration::from_millis(800);

pub fn is_playable_video_frame(frame: &MediaFrame) -> bool {
    if !matches!(frame.codec, CodecType::H264 | CodecType::H265) {
        return false;
    }
    let data = ensure_annex_b(&frame.data);
    !data.is_empty() && !param_only(frame.codec, &data)
}

pub fn is_idr_frame(frame: &MediaFrame) -> bool {
    if frame.codec == CodecType::H265 {
        return is_keyframe_annex_b(frame.codec, &ensure_annex_b(&frame.data));
    }
    frame.is_keyframe || is_keyframe_annex_b(frame.codec, &ensure_annex_b(&frame.data))
}

pub fn prepend_h264_config(
    manager: &StreamManager,
    stream_id: &str,
    frame: &MediaFrame,
) -> Vec<u8> {
    prepend_video_config(manager, stream_id, frame)
}

pub fn prepend_video_config(
    manager: &StreamManager,
    stream_id: &str,
    frame: &MediaFrame,
) -> Vec<u8> {
    let au = ensure_annex_b(&frame.data);
    if !(frame.is_keyframe || is_idr_frame(frame) || contains_sps_or_pps_nalu(&au)) {
        return au;
    }
    if frame.codec == CodecType::H265 {
        // Enhanced RTMP players expect parameter sets in the sequence header only.
        return au;
    }
    if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
        return prepend_annex_b_config(
            frame.codec,
            stream.vps.as_deref(),
            stream.sps.as_deref(),
            stream.pps.as_deref(),
            &au,
        );
    }
    au
}

pub fn prepare_h264_play_frame(
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
        frame.is_keyframe || is_idr_frame(&frame),
        frame.codec,
    )
    .with_optional_clock_rate(frame.clock_rate)
}

/// Jump to live edge and wait for a fresh IDR (do not replay ring history).
pub async fn prime_live_play(
    reader: &mut DispatchReader,
    manager: &StreamManager,
    stream_id: &str,
    log_prefix: &str,
) -> Option<MediaFrame> {
    let started_at = Instant::now();
    reader.finish_prime();
    let requested = request_publisher_keyframe(stream_id);
    info!(
        "[{log_prefix}] [{stream_id}] PLAY prime start latest_seq={} latest_idr={:?} requested_keyframe={}",
        reader.hub().latest_seq(),
        reader.hub().latest_idr_seq(),
        requested
    );

    let deadline = Instant::now() + LIVE_IDR_WAIT;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, reader.recv_batch()).await {
            Ok(Ok(frames)) if !frames.is_empty() => {
                if let Some(idr) = frames
                    .iter()
                    .filter(|f| is_playable_video_frame(f) && is_idr_frame(f))
                    .last()
                {
                    info!(
                        "[{log_prefix}] [{stream_id}] PLAY live IDR after={}ms ts={} bytes={}",
                        started_at.elapsed().as_millis(),
                        idr.timestamp,
                        idr.data.len()
                    );
                    if let Some(idr_seq) = reader.hub().latest_idr_seq() {
                        reader.begin_video_catchup_after_idr(idr_seq, reader.hub().latest_seq());
                    }
                    return Some(prepare_h264_play_frame(manager, stream_id, idr.clone()));
                }
            }
            Ok(Ok(_)) => continue,
            Ok(Err(DispatchError::Closed)) => break,
            Err(_) => break,
        }
    }

    if let (Some(idr), Some(idr_seq)) = (
        reader.hub().latest_idr_frame(),
        reader.hub().latest_idr_seq(),
    ) {
        let frame_lag = reader.hub().latest_seq().saturating_sub(idr_seq);
        let latest = reader.hub().latest_seq();
        if frame_lag <= 30 && is_playable_video_frame(&idr) && is_idr_frame(&idr) {
            reader.begin_video_catchup_after_idr(idr_seq, latest);
            info!(
                "[{log_prefix}] [{stream_id}] PLAY fallback IDR after={}ms ts={} bytes={} seq={} catchup_through={}",
                started_at.elapsed().as_millis(),
                idr.timestamp,
                idr.data.len(),
                idr_seq,
                latest
            );
            return Some(prepare_h264_play_frame(manager, stream_id, idr));
        }
    }

    info!(
        "[{log_prefix}] [{stream_id}] PLAY no IDR after={}ms — wait in relay loop",
        started_at.elapsed().as_millis()
    );
    None
}

/// Block for one frame, coalescing video bursts to the latest playable frame.
pub async fn recv_coalesced_play_frame(
    reader: &mut DispatchReader,
) -> Result<MediaFrame, DispatchError> {
    loop {
        let frame = reader.recv_coalesced().await?;
        if is_playable_video_frame(&frame)
            || !matches!(frame.codec, CodecType::H264 | CodecType::H265)
        {
            return Ok(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{DispatchPolicy, StreamHub};
    use bytes::Bytes;

    fn hevc_idr(ts: u64) -> MediaFrame {
        MediaFrame::new(
            "s".into(),
            0,
            ts,
            Bytes::from_static(&[0, 0, 0, 1, 0x26, 0x01, 0xaf]),
            true,
            CodecType::H265,
        )
    }

    fn hevc_p(ts: u64) -> MediaFrame {
        MediaFrame::new(
            "s".into(),
            0,
            ts,
            Bytes::from_static(&[0, 0, 0, 1, 0x02, 0x01, 0xaf]),
            false,
            CodecType::H265,
        )
    }

    #[tokio::test]
    async fn prime_live_play_preserves_h265_frames_after_idr_in_same_batch() {
        let hub = StreamHub::new("s");
        let mut reader = DispatchReader::new(hub.clone(), DispatchPolicy::LiveSequential);
        let manager = StreamManager::new();

        let publisher = hub.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            publisher.publish(hevc_idr(1));
            publisher.publish(hevc_p(2));
        });

        let idr = prime_live_play(&mut reader, &manager, "s", "TEST")
            .await
            .expect("prime should return fresh H265 IDR");
        assert_eq!(idr.timestamp, 1);

        let batch = reader.recv_batch().await.expect("reader remains open");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].timestamp, 2);
    }
}
