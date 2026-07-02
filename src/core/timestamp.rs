//! Normalize inter-frame deltas from RTMP (ms) vs RTP (90 kHz video clock).

use crate::core::{CodecType, MediaFrame};

/// Minimum tag timestamp step when separate A/V clocks would move backward in mux order.
const FLV_MUX_MIN_STEP_MS: u64 = 1;
const VIDEO_RTP_CLOCK_HZ: u64 = 90_000;
const VIDEO_RTP_TICKS_PER_MS: u64 = VIDEO_RTP_CLOCK_HZ / 1_000;
const RTP_VIDEO_DELTA_MIN_TICKS: u64 = VIDEO_RTP_TICKS_PER_MS * 10;
const RTP_VIDEO_DELTA_MAX_TICKS: u64 = VIDEO_RTP_CLOCK_HZ;
const AAC_DEFAULT_SAMPLE_RATE_HZ: u64 = 44_100;
pub const MILLISECOND_CLOCK_RATE: u32 = 1_000;
pub const VIDEO_RTP_CLOCK_RATE: u32 = VIDEO_RTP_CLOCK_HZ as u32;
pub const AAC_DEFAULT_CLOCK_RATE: u32 = AAC_DEFAULT_SAMPLE_RATE_HZ as u32;

/// Delta between consecutive media timestamps, in milliseconds.
///
/// RTMP publishes millisecond timestamps; RTSP/WebRTC RTP H264 uses a 90 kHz clock
/// (~3600 ticks per 40 ms frame).
pub fn media_timestamp_delta_ms(prev: u64, curr: u64) -> u64 {
    media_timestamp_delta_ms_with_clock(prev, curr, None)
}

pub fn media_frame_timestamp_delta_ms(prev: &MediaFrame, curr: &MediaFrame) -> u64 {
    media_timestamp_delta_ms_with_clock(
        prev.timestamp,
        curr.timestamp,
        curr.clock_rate.or(prev.clock_rate),
    )
}

pub fn media_timestamp_delta_ms_with_clock(prev: u64, curr: u64, clock_rate: Option<u32>) -> u64 {
    if curr <= prev {
        return 0;
    }
    let delta = curr - prev;
    if let Some(clock_rate) = clock_rate {
        if clock_rate == 0 {
            return 0;
        }
        if clock_rate == MILLISECOND_CLOCK_RATE {
            return delta;
        }
        return delta.saturating_mul(1_000) / clock_rate as u64;
    }

    if delta > 2000 || looks_like_rtp_video_delta(delta) {
        delta / VIDEO_RTP_TICKS_PER_MS
    } else {
        delta
    }
}

fn looks_like_rtp_video_delta(delta: u64) -> bool {
    (RTP_VIDEO_DELTA_MIN_TICKS..=RTP_VIDEO_DELTA_MAX_TICKS).contains(&delta)
        && (delta % VIDEO_RTP_TICKS_PER_MS == 0
            || matches!(delta, 1_501 | 1_502 | 3_003 | 3_753 | 3_754))
}

/// FLV tag timestamp in milliseconds (RTMP ms or RTP-derived).
pub fn flv_timestamp_ms(codec: crate::core::CodecType, raw: u64) -> u32 {
    let ms = match codec {
        crate::core::CodecType::H264 | crate::core::CodecType::H265 => {
            if raw > 1_000_000 {
                raw / VIDEO_RTP_TICKS_PER_MS
            } else {
                raw
            }
        }
        crate::core::CodecType::AAC => {
            if raw > 1_000_000 {
                raw * 1000 / AAC_DEFAULT_SAMPLE_RATE_HZ
            } else {
                raw
            }
        }
        _ => raw,
    };
    (ms & 0xFFFF_FFFF) as u32
}

/// Monotonic FLV/RTMP tag timeline in mux order.
///
/// Video and audio keep independent logical clocks; the emitted timestamp is always
/// non-decreasing so interleaved tags never report DTS going backward.
#[derive(Debug, Default)]
pub struct FlvPlayTimeline {
    last_emit_ms: u64,
    video_ms: u64,
    last_raw_video_ts: Option<u64>,
    last_raw_video_clock_rate: Option<u32>,
    audio_ms: u64,
    last_raw_audio_ts: Option<u64>,
    last_raw_audio_clock_rate: Option<u32>,
    audio_frames: u64,
}

impl FlvPlayTimeline {
    pub fn map(&mut self, frame: &MediaFrame) -> u32 {
        let ideal = match frame.codec {
            CodecType::H264 | CodecType::H265 => {
                if let Some(last) = self.last_raw_video_ts {
                    if frame.timestamp > last {
                        let delta = media_timestamp_delta_ms_with_clock(
                            last,
                            frame.timestamp,
                            frame.clock_rate.or(self.last_raw_video_clock_rate),
                        );
                        if delta > 0 && delta < 2000 {
                            self.video_ms += delta;
                        } else {
                            self.video_ms = self.video_ms.saturating_add(FLV_MUX_MIN_STEP_MS);
                        }
                    } else {
                        self.video_ms = self.video_ms.saturating_add(FLV_MUX_MIN_STEP_MS);
                    }
                }
                self.last_raw_video_ts = Some(frame.timestamp);
                self.last_raw_video_clock_rate = frame.clock_rate;
                self.video_ms
            }
            CodecType::AAC => {
                let step_ms =
                    1024 * 1000 / frame.clock_rate.unwrap_or(AAC_DEFAULT_CLOCK_RATE) as u64;
                if let Some(last) = self.last_raw_audio_ts {
                    if frame.timestamp > last {
                        let delta = media_timestamp_delta_ms_with_clock(
                            last,
                            frame.timestamp,
                            frame.clock_rate.or(self.last_raw_audio_clock_rate),
                        );
                        self.audio_ms =
                            self.audio_ms.saturating_add(if delta > 0 && delta < 2000 {
                                delta
                            } else {
                                step_ms
                            });
                    } else {
                        self.audio_ms = self.audio_ms.saturating_add(step_ms);
                    }
                } else {
                    self.audio_ms = self.audio_frames * step_ms;
                }
                self.last_raw_audio_ts = Some(frame.timestamp);
                self.last_raw_audio_clock_rate = frame.clock_rate;
                self.audio_frames += 1;
                self.audio_ms
            }
            _ => self.last_emit_ms,
        };

        let ts = if ideal > self.last_emit_ms {
            ideal
        } else if ideal == self.last_emit_ms && self.last_emit_ms == 0 {
            0
        } else {
            self.last_emit_ms.saturating_add(FLV_MUX_MIN_STEP_MS)
        };
        self.last_emit_ms = ts;
        (ts & 0xFFFF_FFFF) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn h264(ts: u64) -> MediaFrame {
        MediaFrame::new(
            "s".into(),
            0,
            ts,
            Bytes::from_static(b"v"),
            false,
            CodecType::H264,
        )
    }

    fn h264_with_clock(ts: u64, clock_rate: u32) -> MediaFrame {
        h264(ts).with_clock_rate(clock_rate)
    }

    fn aac(n: u64) -> MediaFrame {
        MediaFrame::new(
            "s".into(),
            1,
            n,
            Bytes::from(vec![0u8; 64]),
            false,
            CodecType::AAC,
        )
    }

    fn aac_with_clock(ts: u64, clock_rate: u32) -> MediaFrame {
        aac(ts).with_clock_rate(clock_rate)
    }

    #[test]
    fn rtp_video_delta_to_ms() {
        assert_eq!(media_timestamp_delta_ms(1_690_126_824, 1_690_130_424), 40);
    }

    #[test]
    fn small_rtp_video_delta_uses_rtp_clock_domain() {
        assert_eq!(media_timestamp_delta_ms(2_742_522_000, 2_742_523_800), 20);
    }

    #[test]
    fn fractional_fps_rtp_video_delta_uses_rtp_clock_domain() {
        assert_eq!(media_timestamp_delta_ms(2_742_522_000, 2_742_523_501), 16);
        assert_eq!(media_timestamp_delta_ms(2_742_523_501, 2_742_525_003), 16);
        assert_eq!(media_timestamp_delta_ms(2_742_522_000, 2_742_525_003), 33);
    }

    #[test]
    fn low_base_rtsp_rtp_video_delta_uses_rtp_clock_domain() {
        assert_eq!(media_timestamp_delta_ms(90_000, 91_800), 20);
        assert_eq!(media_timestamp_delta_ms(90_000, 93_600), 40);
    }

    #[test]
    fn rtmp_delta_unchanged() {
        assert_eq!(media_timestamp_delta_ms(1000, 1040), 40);
    }

    #[test]
    fn long_running_rtmp_millisecond_delta_stays_millisecond_domain() {
        assert_eq!(media_timestamp_delta_ms(1_000_000, 1_000_040), 40);
    }

    #[test]
    fn explicit_clock_rate_overrides_timestamp_heuristic() {
        assert_eq!(
            media_timestamp_delta_ms_with_clock(1_000_000, 1_001_000, Some(MILLISECOND_CLOCK_RATE)),
            1000
        );
        assert_eq!(
            media_timestamp_delta_ms_with_clock(90_000, 91_800, Some(VIDEO_RTP_CLOCK_RATE)),
            20
        );
        assert_eq!(
            media_timestamp_delta_ms_with_clock(48_000, 49_024, Some(48_000)),
            21
        );
    }

    #[test]
    fn media_frame_delta_uses_frame_clock_rate() {
        let rtmp_prev = h264_with_clock(1_000_000, MILLISECOND_CLOCK_RATE);
        let rtmp_curr = h264_with_clock(1_001_000, MILLISECOND_CLOCK_RATE);
        assert_eq!(media_frame_timestamp_delta_ms(&rtmp_prev, &rtmp_curr), 1000);

        let rtp_prev = h264_with_clock(90_000, VIDEO_RTP_CLOCK_RATE);
        let rtp_curr = h264_with_clock(91_800, VIDEO_RTP_CLOCK_RATE);
        assert_eq!(media_frame_timestamp_delta_ms(&rtp_prev, &rtp_curr), 20);
    }

    #[test]
    fn flv_timeline_handles_mixed_webrtc_and_rtsp_rtp_deltas() {
        let mut webrtc = FlvPlayTimeline::default();
        assert_eq!(webrtc.map(&h264(2_742_522_000)), 0);
        assert_eq!(webrtc.map(&h264(2_742_523_800)), 20);
        assert_eq!(webrtc.map(&h264(2_742_527_400)), 60);

        let mut rtsp = FlvPlayTimeline::default();
        assert_eq!(rtsp.map(&h264(90_000)), 0);
        assert_eq!(rtsp.map(&h264(91_800)), 20);
        assert_eq!(rtsp.map(&h264(95_400)), 60);
    }

    #[test]
    fn flv_timeline_keeps_rtmp_millisecond_domain_after_long_uptime() {
        let mut tl = FlvPlayTimeline::default();
        assert_eq!(
            tl.map(&h264_with_clock(1_000_000, MILLISECOND_CLOCK_RATE)),
            0
        );
        assert_eq!(
            tl.map(&h264_with_clock(1_000_040, MILLISECOND_CLOCK_RATE)),
            40
        );
        assert_eq!(
            tl.map(&h264_with_clock(1_000_080, MILLISECOND_CLOCK_RATE)),
            80
        );
    }

    #[test]
    fn flv_timeline_uses_audio_clock_rate() {
        let mut tl = FlvPlayTimeline::default();
        assert_eq!(tl.map(&aac_with_clock(48_000, 48_000)), 0);
        assert_eq!(tl.map(&aac_with_clock(49_024, 48_000)), 21);
        assert_eq!(tl.map(&aac_with_clock(50_048, 48_000)), 42);
    }

    #[test]
    fn frame_ring_preserves_clock_rate() {
        let mut ring = crate::core::FrameRing::new();
        ring.push(h264_with_clock(90_000, VIDEO_RTP_CLOCK_RATE));
        let frame = ring.get(0).expect("frame");
        assert_eq!(frame.clock_rate, Some(VIDEO_RTP_CLOCK_RATE));
    }

    #[test]
    fn flv_timeline_never_decreases_when_audio_lags_video() {
        let mut tl = FlvPlayTimeline::default();
        let mut last = 0u32;
        for i in 0..40u64 {
            let ts = tl.map(&h264(i * 40));
            assert!(ts >= last, "video ts {ts} < {last}");
            last = ts;
        }
        // Audio logical clock behind video emit — must not rewind mux DTS.
        let ts = tl.map(&aac(0));
        assert!(ts >= last, "audio ts {ts} < {last}");
        last = ts;
        let ts = tl.map(&h264(40 * 40 + 40));
        assert!(ts > last, "video after audio ts {ts} <= {last}");
    }

    #[test]
    fn flv_timeline_stalls_publisher_ts() {
        let mut tl = FlvPlayTimeline::default();
        let t0 = tl.map(&h264(1000));
        let t1 = tl.map(&h264(1000));
        assert!(t1 >= t0);
    }

    #[test]
    fn hls_mux_timeline_tracks_rtp_gop_without_wall_drift() {
        let base = 2_648_000_000u64;
        let mut last_raw = 0u64;
        let mut mux_ms = 0u64;
        for i in 0..25u64 {
            let ts = base + i * 3600;
            let (new_raw, new_mux) = crate::hls::timing::advance_video_mux_ms(
                last_raw,
                mux_ms,
                ts,
                Some(VIDEO_RTP_CLOCK_RATE),
            );
            last_raw = new_raw;
            mux_ms = new_mux;
        }
        assert!(
            (mux_ms as f64 - 960.0).abs() < 1.0,
            "HLS video mux should follow publisher RTP (~960ms/GOP), got {mux_ms}ms"
        );
    }
}
