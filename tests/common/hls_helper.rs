//! HLS test helper functions.
//!
//! Provides utilities for HLS (HTTP Live Streaming) protocol testing.

use std::path::PathBuf;

/// HLS test helper structure.
#[derive(Debug, Clone)]
pub struct HlsTestHelper {
    stream_id: String,
    output_dir: PathBuf,
}

impl HlsTestHelper {
    /// Create a new HLS test helper.
    pub fn new(stream_id: &str, output_dir: &str) -> Self {
        Self {
            stream_id: stream_id.to_string(),
            output_dir: PathBuf::from(output_dir),
        }
    }

    /// Get the stream ID.
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Get the output directory.
    pub fn output_dir(&self) -> &PathBuf {
        &self.output_dir
    }

    /// Get the master playlist URL (if using adaptive streaming).
    pub fn master_playlist_url(&self, host: &str, port: u16) -> String {
        format!("http://{}:{}/hls/{}/master.m3u8", host, port, self.stream_id)
    }

    /// Get the playlist URL for a specific variant.
    pub fn playlist_url(&self, host: &str, port: u16, variant: &str) -> String {
        format!(
            "http://{}:{}/hls/{}/{}.m3u8",
            host, port, self.stream_id, variant
        )
    }

    /// Get the segment URL.
    pub fn segment_url(&self, host: &str, port: u16, variant: &str, segment: &str) -> String {
        format!(
            "http://{}:{}/hls/{}/{}/{}",
            host, port, self.stream_id, variant, segment
        )
    }

    /// Get local path to master playlist.
    pub fn master_playlist_path(&self) -> PathBuf {
        self.output_dir.join(&self.stream_id).join("master.m3u8")
    }

    /// Get local path to playlist.
    pub fn playlist_path(&self, variant: &str) -> PathBuf {
        self.output_dir.join(&self.stream_id).join(format!("{}.m3u8", variant))
    }

    /// Get local path to segment.
    pub fn segment_path(&self, variant: &str, segment: &str) -> PathBuf {
        self.output_dir
            .join(&self.stream_id)
            .join(variant)
            .join(segment)
    }
}

/// Parse m3u8 playlist header.
pub fn parse_m3u8_header(data: &str) -> Option<&str> {
    data.lines().next()
}

/// Check if file is a valid m3u8 playlist.
pub fn is_valid_m3u8(data: &str) -> bool {
    parse_m3u8_header(data).map(|h| h == "#EXTM3U").unwrap_or(false)
}

/// Parse m3u8 playlist info.
pub fn parse_m3u8_info(data: &str) -> M3u8Info {
    let mut info = M3u8Info::default();

    for line in data.lines() {
        let line = line.trim();
        if line.starts_with("#EXT-X-VERSION:") {
            if let Some(v) = line.strip_prefix("#EXT-X-VERSION:") {
                info.version = v.parse().ok();
            }
        } else if line.starts_with("#EXT-X-TARGETDURATION:") {
            if let Some(v) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
                info.target_duration = v.parse().ok();
            }
        } else if line.starts_with("#EXT-X-MEDIA-SEQUENCE:") {
            if let Some(v) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
                info.media_sequence = v.parse().ok();
            }
        } else if line.starts_with("#EXT-X-ENDLIST") {
            info.is_live = false;
        } else if line.ends_with(".ts") || line.ends_with(".m4s") {
            info.segments.push(line.to_string());
        }
    }

    info
}

/// M3U8 playlist information.
#[derive(Debug, Default)]
pub struct M3u8Info {
    pub version: Option<u8>,
    pub target_duration: Option<u32>,
    pub media_sequence: Option<u64>,
    pub is_live: bool,
    pub segments: Vec<String>,
}

impl M3u8Info {
    /// Check if this is a live playlist (no ENDLIST tag).
    pub fn is_live(&self) -> bool {
        self.is_live
    }

    /// Get the number of segments.
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Check if playlist has minimum required segments for playback.
    pub fn is_playable(&self) -> bool {
        !self.segments.is_empty()
    }
}

/// Parse segment duration from EXTINF tag.
pub fn parse_extinf_duration(line: &str) -> Option<f64> {
    let line = line.trim();
    if !line.starts_with("#EXTINF:") {
        return None;
    }

    let value = line.strip_prefix("#EXTINF:")?;
    let duration_str = value.split(',').next()?;
    duration_str.trim().parse().ok()
}

/// Default HLS segment duration in seconds.
pub const DEFAULT_SEGMENT_DURATION: u32 = 2;

/// Default maximum segments in playlist.
pub const DEFAULT_MAX_SEGMENTS: usize = 10;

/// Generate a simple test master playlist.
pub fn generate_master_playlist(variants: &[(&str, u32, u32)]) -> String {
    let mut playlist = String::from("#EXTM3U\n\n");

    for (name, bandwidth, resolution) in variants {
        playlist.push_str(&format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={},RESOLUTION={}x{}\n",
            bandwidth,
            resolution / 100,
            resolution % 100
        ));
        playlist.push_str(&format!("{}.m3u8\n\n", name));
    }

    playlist
}

/// Generate a simple test media playlist.
pub fn generate_media_playlist(
    segments: &[(&str, f64)],
    target_duration: u32,
    media_sequence: u64,
) -> String {
    let mut playlist = String::new();

    playlist.push_str("#EXTM3U\n");
    playlist.push_str(&format!("#EXT-X-VERSION:3\n"));
    playlist.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", target_duration));
    playlist.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{}\n", media_sequence));

    for (segment, duration) in segments {
        playlist.push_str(&format!("#EXTINF:{:.3},\n", duration));
        playlist.push_str(&format!("{}\n", segment));
    }

    playlist
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hls_helper() {
        let helper = HlsTestHelper::new("test", "/tmp/hls");
        assert_eq!(helper.stream_id(), "test");
    }

    #[test]
    fn test_is_valid_m3u8() {
        assert!(is_valid_m3u8("#EXTM3U\ntest.ts\n"));
        assert!(!is_valid_m3u8("test.ts\n"));
    }

    #[test]
    fn test_parse_m3u8_info() {
        let playlist = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:2
#EXT-X-MEDIA-SEQUENCE:1
#EXTINF:2.000,
segment1.ts
#EXTINF:2.000,
segment2.ts
#EXT-X-ENDLIST
"#;
        let info = parse_m3u8_info(playlist);
        assert_eq!(info.version, Some(3));
        assert_eq!(info.target_duration, Some(2));
        assert_eq!(info.media_sequence, Some(1));
        assert_eq!(info.segment_count(), 2);
        assert!(!info.is_live());
    }

    #[test]
    fn test_parse_extinf_duration() {
        assert_eq!(parse_extinf_duration("#EXTINF:2.000,"), Some(2.0));
        assert_eq!(parse_extinf_duration("#EXTINF:1.500,live"), Some(1.5));
        assert_eq!(parse_extinf_duration("#EXT-X-TARGETDURATION:2"), None);
    }

    #[test]
    fn test_generate_master_playlist() {
        let playlist = generate_master_playlist(&[
            ("low", 500000, 320200),    // 320x200
            ("high", 2000000, 1280960), // 1280x960
        ]);
        assert!(playlist.contains("BANDWIDTH=500000"));
        assert!(playlist.contains("low.m3u8"));
        assert!(playlist.contains("high.m3u8"));
    }
}
