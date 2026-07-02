use serde::Deserialize;
use std::path::PathBuf;

pub const DEFAULT_STORAGE_BASE_DIR: &str = "./saving";
pub const DEFAULT_HLS_OUTPUT_DIR: &str = "hls";
pub const DEFAULT_RECORD_OUTPUT_DIR: &str = "recordings";
pub const DEFAULT_SNAPSHOT_OUTPUT_DIR: &str = "snapshots";

/// Legacy full-path defaults used by module `Default` impls when no config is provided.
pub const DEFAULT_HLS_DIR: &str = "./saving/hls";
pub const DEFAULT_RECORD_DIR: &str = "./saving/recordings";
pub const DEFAULT_SNAPSHOT_DIR: &str = "./saving/snapshots";

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub storage: Option<StorageConfig>,
    pub record: Option<RecordConfig>,
    pub analysis: Option<AnalysisConfig>,
    pub snapshot: Option<SnapshotConfig>,
    pub log: LogConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub rtmp: RtmpConfig,
    pub rtsp: RtspConfig,
    pub webrtc: WebrtcConfig,
    pub http: HttpConfig,
    pub hls: Option<HlsConfig>,
    pub http_flv: Option<HttpFlvConfig>,
}

/// Common storage root; module `output_dir` values are joined under this path.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StorageConfig {
    pub base_dir: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RtmpConfig {
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RtspConfig {
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebrtcConfig {
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpConfig {
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HlsConfig {
    pub enabled: bool,
    pub segment_duration: Option<f64>,
    pub max_segments: Option<usize>,
    pub output_dir: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpFlvConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordConfig {
    pub enabled: bool,
    pub output_dir: Option<String>,
    pub default_format: Option<String>,
    pub segment_duration_sec: Option<u64>,
    pub align_keyframe: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnalysisConfig {
    pub enabled: bool,
    pub default_sample_interval: Option<u64>,
    pub max_events_per_stream: Option<usize>,
    pub ffmpeg_path: Option<String>,
    pub face_detection_dir: Option<String>,
    pub face_detection_interval_ms: Option<u64>,
    pub face_detector_command: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotConfig {
    pub enabled: bool,
    pub output_dir: Option<String>,
    pub ffmpeg_path: Option<String>,
    pub wait_keyframe_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogConfig {
    pub level: String,
    pub path: String,
    pub max_size_mb: u64,
    pub max_files: usize,
    pub modules: Option<std::collections::HashMap<String, String>>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            path: "./logs/media-server.log".to_string(),
            max_size_mb: 10,
            max_files: 5,
            modules: None,
        }
    }
}

impl Config {
    fn storage_base_dir(&self) -> &str {
        self.storage
            .as_ref()
            .and_then(|s| s.base_dir.as_deref())
            .unwrap_or(DEFAULT_STORAGE_BASE_DIR)
    }

    fn resolve_output_dir(output_dir: Option<&str>, default_name: &str, base_dir: &str) -> PathBuf {
        let output = output_dir.unwrap_or(default_name);
        let path = PathBuf::from(output);
        if path.is_absolute() {
            path
        } else {
            PathBuf::from(base_dir).join(path)
        }
    }

    pub fn hls_output_dir(&self) -> String {
        Self::resolve_output_dir(
            self.server
                .hls
                .as_ref()
                .and_then(|h| h.output_dir.as_deref()),
            DEFAULT_HLS_OUTPUT_DIR,
            self.storage_base_dir(),
        )
        .to_string_lossy()
        .into_owned()
    }

    pub fn record_output_dir(&self) -> PathBuf {
        Self::resolve_output_dir(
            self.record.as_ref().and_then(|r| r.output_dir.as_deref()),
            DEFAULT_RECORD_OUTPUT_DIR,
            self.storage_base_dir(),
        )
    }

    pub fn snapshot_output_dir(&self) -> PathBuf {
        Self::resolve_output_dir(
            self.snapshot.as_ref().and_then(|s| s.output_dir.as_deref()),
            DEFAULT_SNAPSHOT_OUTPUT_DIR,
            self.storage_base_dir(),
        )
    }

    pub fn ensure_storage_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.hls_output_dir())?;
        std::fs::create_dir_all(&self.record_output_dir())?;
        std::fs::create_dir_all(&self.snapshot_output_dir())?;
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                rtmp: RtmpConfig { port: 1935 },
                rtsp: RtspConfig { port: 554 },
                webrtc: WebrtcConfig { port: 9080 },
                http: HttpConfig { port: 8081 },
                hls: Some(HlsConfig {
                    enabled: true,
                    segment_duration: Some(1.0),
                    max_segments: Some(1),
                    output_dir: Some(DEFAULT_HLS_OUTPUT_DIR.to_string()),
                }),
                http_flv: Some(HttpFlvConfig { enabled: true }),
            },
            storage: Some(StorageConfig {
                base_dir: Some(DEFAULT_STORAGE_BASE_DIR.to_string()),
            }),
            record: Some(RecordConfig {
                enabled: false,
                output_dir: Some(DEFAULT_RECORD_OUTPUT_DIR.to_string()),
                default_format: Some("ts".to_string()),
                segment_duration_sec: Some(300),
                align_keyframe: Some(true),
            }),
            analysis: Some(AnalysisConfig {
                enabled: false,
                default_sample_interval: Some(1),
                max_events_per_stream: Some(256),
                ffmpeg_path: Some("ffmpeg".to_string()),
                face_detection_dir: Some("./analysis".to_string()),
                face_detection_interval_ms: Some(1_000),
                face_detector_command: None,
            }),
            snapshot: Some(SnapshotConfig {
                enabled: false,
                output_dir: Some(DEFAULT_SNAPSHOT_OUTPUT_DIR.to_string()),
                ffmpeg_path: Some("ffmpeg".to_string()),
                wait_keyframe_ms: Some(1_000),
            }),
            log: LogConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_output_dir_joins_storage_base_dir() {
        let config: Config = toml::from_str(
            r#"
[server.rtmp]
port = 1935
[server.rtsp]
port = 554
[server.webrtc]
port = 9080
[server.http]
port = 8081
[storage]
base_dir = "/data"
[server.hls]
enabled = true
output_dir = "hls"
[record]
enabled = true
output_dir = "recordings"
[snapshot]
enabled = true
output_dir = "snapshots"
[log]
level = "info"
path = "./logs/media-server.log"
max_size_mb = 10
max_files = 5
"#,
        )
        .unwrap();

        assert_eq!(config.hls_output_dir(), "/data/hls");
        assert_eq!(config.record_output_dir(), PathBuf::from("/data/recordings"));
        assert_eq!(config.snapshot_output_dir(), PathBuf::from("/data/snapshots"));
    }

    #[test]
    fn absolute_output_dir_skips_base_dir_join() {
        let config: Config = toml::from_str(
            r#"
[server.rtmp]
port = 1935
[server.rtsp]
port = 554
[server.webrtc]
port = 9080
[server.http]
port = 8081
[storage]
base_dir = "/data"
[server.hls]
enabled = true
output_dir = "/custom/hls"
[record]
enabled = true
output_dir = "/custom/recordings"
[snapshot]
enabled = true
output_dir = "/custom/snapshots"
[log]
level = "info"
path = "./logs/media-server.log"
max_size_mb = 10
max_files = 5
"#,
        )
        .unwrap();

        assert_eq!(config.hls_output_dir(), "/custom/hls");
        assert_eq!(config.record_output_dir(), PathBuf::from("/custom/recordings"));
        assert_eq!(config.snapshot_output_dir(), PathBuf::from("/custom/snapshots"));
    }

    #[test]
    fn default_subdirs_used_when_output_dir_missing() {
        let config: Config = toml::from_str(
            r#"
[server.rtmp]
port = 1935
[server.rtsp]
port = 554
[server.webrtc]
port = 9080
[server.http]
port = 8081
[storage]
base_dir = "./saving"
[log]
level = "info"
path = "./logs/media-server.log"
max_size_mb = 10
max_files = 5
"#,
        )
        .unwrap();

        assert_eq!(config.hls_output_dir(), "./saving/hls");
        assert_eq!(
            config.record_output_dir(),
            PathBuf::from("./saving/recordings")
        );
        assert_eq!(
            config.snapshot_output_dir(),
            PathBuf::from("./saving/snapshots")
        );
    }
}
