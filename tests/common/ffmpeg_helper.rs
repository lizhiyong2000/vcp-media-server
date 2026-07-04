//! FFmpeg test helper functions.
//!
//! Provides utilities for simulating real streaming using ffmpeg testsrc
//! and ffplay for cross-protocol testing.

use super::TestCodec;

/// FFmpeg testsrc publish configuration.
#[derive(Debug, Clone)]
pub struct FfmpegTestConfig {
    /// Stream resolution.
    pub width: u32,
    pub height: u32,
    /// Frame rate.
    pub framerate: u32,
    /// Test duration in seconds (0 = infinite).
    pub duration_secs: u32,
    /// Video bitrate in kbps.
    pub video_bitrate: u32,
    /// Audio bitrate in kbps.
    pub audio_bitrate: u32,
}

impl Default for FfmpegTestConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 480,
            framerate: 25,
            duration_secs: 5,
            video_bitrate: 1000,
            audio_bitrate: 64,
        }
    }
}

impl FfmpegTestConfig {
    /// Create a 480p test config.
    pub fn sd() -> Self {
        Self::default()
    }

    /// Create a 720p test config.
    pub fn hd() -> Self {
        Self {
            width: 1280,
            height: 720,
            framerate: 30,
            duration_secs: 5,
            video_bitrate: 3000,
            audio_bitrate: 128,
        }
    }

    /// Create a 1080p test config.
    pub fn full_hd() -> Self {
        Self {
            width: 1920,
            height: 1080,
            framerate: 30,
            duration_secs: 5,
            video_bitrate: 5000,
            audio_bitrate: 192,
        }
    }

    /// Get codec-specific pixel format.
    pub fn pixel_format(&self, codec: TestCodec) -> &'static str {
        match codec {
            TestCodec::H264 => "yuv420p",
            TestCodec::H265 => "yuv420p",
        }
    }
}

/// Generate ffmpeg testsrc publish command for RTMP.
pub fn create_rtmp_publish_command(
    stream_url: &str,
    codec: TestCodec,
    config: &FfmpegTestConfig,
) -> Vec<String> {
    let codec_name = match codec {
        TestCodec::H264 => "libx264",
        TestCodec::H265 => "libx265",
    };

    let duration = if config.duration_secs > 0 {
        config.duration_secs.to_string()
    } else {
        "30".to_string()
    };

    let mut args = vec![
        "ffmpeg".to_string(),
        "-re".to_string(),
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        format!("testsrc2=size={}x{}:rate={}", config.width, config.height, config.framerate),
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        "aevalsrc=0.1*sin(2*PI*440*t):sample_rate=48000".to_string(),
        "-c:v".to_string(),
        codec_name.to_string(),
    ];

    if matches!(codec, TestCodec::H265) {
        args.push("-preset".to_string());
        args.push("ultrafast".to_string());
    }

    args.extend([
        "-c:a".to_string(),
        "aac".to_string(),
        "-f".to_string(),
        "flv".to_string(),
        "-t".to_string(),
        duration,
        "-y".to_string(),
        stream_url.to_string(),
    ]);

    args
}

/// Generate simplified ffmpeg testsrc publish command for RTMP.
pub fn create_simple_rtmp_publish_command(
    stream_url: &str,
    codec: TestCodec,
    config: &FfmpegTestConfig,
) -> Vec<String> {
    create_rtmp_publish_command(stream_url, codec, config)
}

/// Generate ffmpeg RTSP publish command.
pub fn create_rtsp_publish_command(
    stream_url: &str,
    codec: TestCodec,
    config: &FfmpegTestConfig,
) -> Vec<String> {
    let codec_name = match codec {
        TestCodec::H264 => "libx264",
        TestCodec::H265 => "libx265",
    };

    let duration = if config.duration_secs > 0 {
        config.duration_secs.to_string()
    } else {
        "30".to_string()
    };

    let mut args = vec![
        "ffmpeg".to_string(),
        "-re".to_string(),
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        format!("testsrc2=size={}x{}:rate={}", config.width, config.height, config.framerate),
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        "aevalsrc=0.1*sin(2*PI*440*t):sample_rate=48000".to_string(),
        "-c:v".to_string(),
        codec_name.to_string(),
    ];

    if matches!(codec, TestCodec::H265) {
        args.push("-preset".to_string());
        args.push("ultrafast".to_string());
    }

    args.extend([
        "-c:a".to_string(),
        "aac".to_string(),
        "-f".to_string(),
        "rtsp".to_string(),
        "-t".to_string(),
        duration,
        "-y".to_string(),
        stream_url.to_string(),
    ]);

    args
}

/// Generate ffplay command for RTMP playback.
pub fn create_rtmp_play_command(stream_url: &str, duration_secs: u32) -> Vec<String> {
    let duration = if duration_secs > 0 {
        duration_secs.to_string()
    } else {
        "10".to_string()
    };

    vec![
        "ffplay".to_string(),
        "-i".to_string(),
        stream_url.to_string(),
        "-t".to_string(),
        duration,
        "-autoexit".to_string(),
        "-nodisp".to_string(),
    ]
}

/// Generate ffplay command for RTSP playback.
pub fn create_rtsp_play_command(
    stream_url: &str,
    duration_secs: u32,
    tcp_mode: bool,
) -> Vec<String> {
    let duration = if duration_secs > 0 {
        duration_secs.to_string()
    } else {
        "10".to_string()
    };

    let mut cmd = vec![
        "ffplay".to_string(),
    ];

    if tcp_mode {
        cmd.push("-rtsp_transport".to_string());
        cmd.push("tcp".to_string());
    }

    cmd.extend([
        "-i".to_string(),
        stream_url.to_string(),
        "-t".to_string(),
        duration,
        "-autoexit".to_string(),
        "-nodisp".to_string(),
    ]);

    cmd
}

/// Generate ffplay command for HTTP-FLV playback.
pub fn create_flv_play_command(stream_url: &str, duration_secs: u32) -> Vec<String> {
    let duration = if duration_secs > 0 {
        duration_secs.to_string()
    } else {
        "10".to_string()
    };

    vec![
        "ffplay".to_string(),
        "-i".to_string(),
        stream_url.to_string(),
        "-t".to_string(),
        duration,
        "-autoexit".to_string(),
        "-nodisp".to_string(),
    ]
}

/// Generate ffplay command for HLS playback.
pub fn create_hls_play_command(m3u8_url: &str, duration_secs: u32) -> Vec<String> {
    let duration = if duration_secs > 0 {
        duration_secs.to_string()
    } else {
        "10".to_string()
    };

    vec![
        "ffplay".to_string(),
        "-i".to_string(),
        m3u8_url.to_string(),
        "-t".to_string(),
        duration,
        "-autoexit".to_string(),
        "-nodisp".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtmp_publish_command() {
        let url = "rtmp://localhost:1935/live/test";
        let config = FfmpegTestConfig::sd();
        let cmd = create_rtmp_publish_command(url, TestCodec::H264, &config);

        assert_eq!(cmd[0], "ffmpeg");
        assert!(cmd.contains(&"-f".to_string()));
        assert!(cmd.contains(&"lavfi".to_string()));
        assert!(cmd.contains(&"-c:v".to_string()));
        assert!(cmd.contains(&"libx264".to_string()));
        assert!(cmd.contains(&"-f".to_string()));
        assert!(cmd.contains(&"flv".to_string()));
        assert_eq!(cmd.last().unwrap().as_str(), url);
    }

    #[test]
    fn test_rtsp_publish_command() {
        let url = "rtsp://localhost:554/stream";
        let config = FfmpegTestConfig::hd();
        let cmd = create_rtsp_publish_command(url, TestCodec::H265, &config);

        assert_eq!(cmd[0], "ffmpeg");
        assert!(cmd.contains(&"libx265".to_string()));
        assert!(cmd.contains(&"-f".to_string()));
        assert!(cmd.contains(&"rtsp".to_string()));
    }

    #[test]
    fn test_rtmp_play_command() {
        let url = "rtmp://localhost:1935/live/test";
        let cmd = create_rtmp_play_command(url, 5);

        assert_eq!(cmd[0], "ffplay");
        assert!(cmd.contains(&"-i".to_string()));
        assert!(cmd.contains(&"-t".to_string()));
        assert!(cmd.contains(&"5".to_string()));
        assert!(cmd.contains(&"-nodisp".to_string()));
    }

    #[test]
    fn test_rtsp_play_command_tcp() {
        let url = "rtsp://localhost:554/stream";
        let cmd = create_rtsp_play_command(url, 10, true);

        assert_eq!(cmd[0], "ffplay");
        assert!(cmd.contains(&"-rtsp_transport".to_string()));
        assert!(cmd.contains(&"tcp".to_string()));
    }

    #[test]
    fn test_flv_play_command() {
        let url = "http://localhost:8081/flv/test";
        let cmd = create_flv_play_command(url, 5);

        assert_eq!(cmd[0], "ffplay");
        assert!(cmd.contains(&"-i".to_string()));
    }

    #[test]
    fn test_config_variants() {
        let sd = FfmpegTestConfig::sd();
        assert_eq!(sd.width, 640);
        assert_eq!(sd.height, 480);

        let hd = FfmpegTestConfig::hd();
        assert_eq!(hd.width, 1280);
        assert_eq!(hd.height, 720);

        let fhd = FfmpegTestConfig::full_hd();
        assert_eq!(fhd.width, 1920);
        assert_eq!(fhd.height, 1080);
    }
}
