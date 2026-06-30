//! Normalize inter-frame deltas from RTMP (ms) vs RTP (90 kHz video clock).

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtp_video_delta_to_ms() {
        assert_eq!(media_timestamp_delta_ms(1_690_126_824, 1_690_130_424), 40);
    }

    #[test]
    fn rtmp_delta_unchanged() {
        assert_eq!(media_timestamp_delta_ms(1000, 1040), 40);
    }
}
