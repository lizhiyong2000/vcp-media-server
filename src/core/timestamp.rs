//! Normalize inter-frame deltas from RTMP (ms) vs RTP (90 kHz video clock).

use crate::core::{CodecType, MediaFrame};

/// Minimum tag timestamp step when separate A/V clocks would move backward in mux order.
const FLV_MUX_MIN_STEP_MS: u64 = 1;

/// Delta between consecutive media timestamps, in milliseconds.
///
/// RTMP publishes millisecond timestamps; RTSP/RTP H264 uses a 90 kHz clock
/// (~3600 ticks per 40 ms frame). Values above 2 s are treated as RTP ticks.
pub fn media_timestamp_delta_ms(prev: u64, curr: u64) -> u64 {
    if curr <= prev {
        return 0;
    }
    let delta = curr - prev;
    if delta > 2000 {
        delta / 90
    } else {
        delta
    }
}

/// FLV tag timestamp in milliseconds (RTMP ms or RTP-derived).
pub fn flv_timestamp_ms(codec: crate::core::CodecType, raw: u64) -> u32 {
    let ms = match codec {
        crate::core::CodecType::H264 | crate::core::CodecType::H265 => {
            if raw > 1_000_000 {
                raw / 90
            } else {
                raw
            }
        }
        crate::core::CodecType::AAC => {
            if raw > 1_000_000 {
                raw * 1000 / 44100
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
    audio_frames: u64,
}

impl FlvPlayTimeline {
    pub fn map(&mut self, frame: &MediaFrame) -> u32 {
        let ideal = match frame.codec {
            CodecType::H264 | CodecType::H265 => {
                if let Some(last) = self.last_raw_video_ts {
                    if frame.timestamp > last {
                        let delta = media_timestamp_delta_ms(last, frame.timestamp);
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
                self.video_ms
            }
            CodecType::AAC => {
                let ms = self.audio_frames * 1024 * 1000 / 44100;
                self.audio_frames += 1;
                ms
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

    #[test]
    fn rtp_video_delta_to_ms() {
        assert_eq!(media_timestamp_delta_ms(1_690_126_824, 1_690_130_424), 40);
    }

    #[test]
    fn rtmp_delta_unchanged() {
        assert_eq!(media_timestamp_delta_ms(1000, 1040), 40);
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
            let (new_raw, new_mux) = crate::hls::timing::advance_video_mux_ms(last_raw, mux_ms, ts);
            last_raw = new_raw;
            mux_ms = new_mux;
        }
        assert!(
            (mux_ms as f64 - 960.0).abs() < 1.0,
            "HLS video mux should follow publisher RTP (~960ms/GOP), got {mux_ms}ms"
        );
    }
}
