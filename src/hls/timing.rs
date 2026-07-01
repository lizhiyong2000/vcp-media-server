//! HLS segment timing: EXTINF, split decisions, and live-edge PDT helpers.

use std::time::{Duration, SystemTime};

use crate::core::media_timestamp_delta_ms;

/// Split when mux or publisher span reaches this fraction of target (25fps GOP ≈ 960ms).
pub const SPLIT_THRESHOLD_RATIO: f64 = 0.85;
/// Minimum completed segment media duration for EXTINF.
pub const MIN_SEGMENT_DURATION: f64 = 0.25;
/// First committed segment may close after this mux span (seconds).
pub const FIRST_SEGMENT_MUX_SECS: f64 = 0.35;
/// Fallback video step when publisher timestamps stall (25 fps).
pub const MIN_VIDEO_MUX_STEP_MS: u64 = 40;

/// Closed segment EXTINF from mux PTS span (must match TS payload).
pub fn closed_segment_secs(last_mux_ms: u64, open_mux_ms_at_split: u64) -> f64 {
    let span_ms = last_mux_ms.saturating_sub(open_mux_ms_at_split);
    (span_ms as f64 / 1000.0).max(MIN_SEGMENT_DURATION)
}

pub fn split_threshold_secs(target_duration: f64, committed_segments: usize) -> f64 {
    if committed_segments == 0 {
        FIRST_SEGMENT_MUX_SECS
    } else {
        target_duration
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SplitEval {
    pub is_keyframe: bool,
    pub mux_secs: f64,
    pub publisher_secs: f64,
    pub split_threshold: f64,
    pub committed_segments: usize,
    pub has_muxed_media: bool,
    pub buffer_len: usize,
    pub min_segment_bytes: usize,
}

/// Whether to roll the current segment on this IDR.
pub fn should_split_segment(input: &SplitEval) -> bool {
    if !input.is_keyframe || !input.has_muxed_media || input.buffer_len <= input.min_segment_bytes {
        return false;
    }

    let ready_by_time = input.mux_secs >= input.split_threshold * SPLIT_THRESHOLD_RATIO
        || input.publisher_secs >= input.split_threshold * SPLIT_THRESHOLD_RATIO;
    let force_on_long_gop = input.mux_secs >= input.split_threshold * 1.5
        || input.publisher_secs >= input.split_threshold * 1.5;
    let first_segment = input.committed_segments == 0 && input.mux_secs >= FIRST_SEGMENT_MUX_SECS;

    ready_by_time || force_on_long_gop || first_segment
}

/// Advance session video mux ms from publisher timestamps (not wall clock).
pub fn advance_video_mux_ms(
    session_last_raw_video: u64,
    session_video_mux_ms: u64,
    frame_timestamp: u64,
) -> (u64, u64) {
    let step = if session_last_raw_video > 0 && frame_timestamp > session_last_raw_video {
        let delta = media_timestamp_delta_ms(session_last_raw_video, frame_timestamp);
        if delta > 0 && delta < 500 {
            delta
        } else {
            MIN_VIDEO_MUX_STEP_MS
        }
    } else if session_video_mux_ms == 0 {
        0
    } else {
        MIN_VIDEO_MUX_STEP_MS
    };

    let new_mux_ms = if session_video_mux_ms == 0 && step == 0 {
        0
    } else {
        session_video_mux_ms.saturating_add(if step == 0 {
            MIN_VIDEO_MUX_STEP_MS
        } else {
            step
        })
    };

    (frame_timestamp, new_mux_ms)
}

/// PDT = wall now minus segment duration (live edge).
pub fn live_pdt(duration: f64, now: SystemTime) -> SystemTime {
    now.checked_sub(Duration::from_secs_f64(duration.max(0.0)))
        .unwrap_or(now)
}

/// Seconds between PDT and wall now; should match EXTINF for a just-committed segment.
pub fn pdt_to_live_edge_secs(pdt: SystemTime, now: SystemTime) -> f64 {
    now.duration_since(pdt).unwrap_or_default().as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_segment_secs_uses_mux_span_not_cap() {
        assert!((closed_segment_secs(2960, 0) - 2.96).abs() < 0.001);
        assert!((closed_segment_secs(250, 0) - MIN_SEGMENT_DURATION).abs() < 0.001);
    }

    #[test]
    fn splits_at_960ms_gop_when_target_is_one_second() {
        let input = SplitEval {
            is_keyframe: true,
            mux_secs: 0.96,
            publisher_secs: 0.96,
            split_threshold: 1.0,
            committed_segments: 1,
            has_muxed_media: true,
            buffer_len: 1024,
            min_segment_bytes: 512,
        };
        assert!(
            should_split_segment(&input),
            "960ms GOP must split at 85% of 1s target"
        );
    }

    #[test]
    fn does_not_split_at_960ms_when_not_keyframe() {
        let input = SplitEval {
            is_keyframe: false,
            mux_secs: 0.96,
            publisher_secs: 0.96,
            split_threshold: 1.0,
            committed_segments: 1,
            has_muxed_media: true,
            buffer_len: 1024,
            min_segment_bytes: 512,
        };
        assert!(!should_split_segment(&input));
    }

    #[test]
    fn does_not_wait_for_three_seconds_at_sub_threshold_gop() {
        let input = SplitEval {
            is_keyframe: true,
            mux_secs: 0.96,
            publisher_secs: 0.96,
            split_threshold: 1.0,
            committed_segments: 1,
            has_muxed_media: true,
            buffer_len: 1024,
            min_segment_bytes: 512,
        };
        assert!(
            should_split_segment(&input),
            "must not require 3s accumulation for 1s GOP"
        );

        let below = SplitEval {
            mux_secs: 0.80,
            publisher_secs: 0.80,
            ..input
        };
        assert!(!should_split_segment(&below));
    }

    #[test]
    fn rtp_one_second_steps_to_one_second_mux_timeline() {
        let base = 2_648_000_000u64;
        let mut last_raw = 0u64;
        let mut mux_ms = 0u64;

        for i in 0..25u64 {
            let ts = base + i * 3600;
            let (new_raw, new_mux) = advance_video_mux_ms(last_raw, mux_ms, ts);
            last_raw = new_raw;
            mux_ms = new_mux;
        }

        assert!(
            (mux_ms as f64 - 960.0).abs() < 1.0,
            "25 frames @ 40ms should land near 960ms, got {mux_ms}"
        );
    }

    #[test]
    fn mux_timeline_uses_publisher_delta_not_wall_jump() {
        let (_, mux0) = advance_video_mux_ms(0, 0, 1_000_000);
        assert_eq!(mux0, 0);

        let (_, mux1) = advance_video_mux_ms(1_000_000, mux0, 1_000_040);
        assert_eq!(mux1, 40);

        // Stall then single frame — only one step, not wall elapsed.
        let (_, mux2) = advance_video_mux_ms(1_000_040, mux1, 1_000_040);
        assert_eq!(mux2, 80);
    }

    #[test]
    fn live_pdt_aligns_to_recent_wall_edge() {
        let now = SystemTime::now();
        let duration = 0.96;
        let pdt = live_pdt(duration, now);
        let edge = pdt_to_live_edge_secs(pdt, now);
        assert!(
            (edge - duration).abs() < 0.05,
            "PDT should be ~duration behind now, got edge={edge}"
        );
    }

    #[test]
    fn extinf_matches_pts_span_for_typical_segment() {
        let open = 49_000u64;
        let last = 50_960u64;
        let extinf = closed_segment_secs(last, open);
        assert!(
            (extinf - 1.96).abs() < 0.01,
            "EXTINF must reflect mux span, got {extinf}"
        );
    }
}
