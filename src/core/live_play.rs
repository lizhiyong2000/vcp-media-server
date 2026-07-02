//! Low-latency live play: snap to live edge, wait for fresh IDR, coalesce bursts.

use std::time::{Duration, Instant};

use tracing::info;

use super::dispatch::{last_playable_idr_seq, DispatchError, DispatchReader};
use super::{CodecType, MediaFrame, StreamManager};
use crate::server::webrtc::h264_util::{
    contains_idr_nalu, contains_sps_or_pps_nalu, ensure_annex_b, is_parameter_set_only,
};
use crate::server::webrtc::{annex_b_with_config, request_publisher_keyframe};

const LIVE_IDR_WAIT: Duration = Duration::from_millis(800);

pub fn is_playable_video_frame(frame: &MediaFrame) -> bool {
    if !matches!(frame.codec, CodecType::H264 | CodecType::H265) {
        return false;
    }
    let data = ensure_annex_b(&frame.data);
    !data.is_empty() && !is_parameter_set_only(&data)
}

pub fn is_idr_frame(frame: &MediaFrame) -> bool {
    frame.is_keyframe || contains_idr_nalu(&ensure_annex_b(&frame.data))
}

pub fn prepend_h264_config(
    manager: &StreamManager,
    stream_id: &str,
    frame: &MediaFrame,
) -> Vec<u8> {
    let au = ensure_annex_b(&frame.data);
    if !(frame.is_keyframe || is_idr_frame(frame) || contains_sps_or_pps_nalu(&au)) {
        return au;
    }
    if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
        if let (Some(sps), Some(pps)) = (&stream.sps, &stream.pps) {
            return annex_b_with_config(sps, pps, &au);
        }
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
        true,
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
