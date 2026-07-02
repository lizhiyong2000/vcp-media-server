use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub rtmp: RtmpConfig,
    pub rtsp: RtspConfig,
    pub webrtc: WebrtcConfig,
    pub http: HttpConfig,
    pub hls: Option<HlsConfig>,
    pub http_flv: Option<HttpFlvConfig>,
    pub record: Option<RecordConfig>,
    pub analysis: Option<AnalysisConfig>,
    pub snapshot: Option<SnapshotConfig>,
    pub streams: Vec<StreamConfig>,
    pub log: LogConfig,
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
    pub base_dir: Option<String>,
    pub default_format: Option<String>,
    pub segment_duration_sec: Option<u64>,
    pub align_keyframe: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnalysisConfig {
    pub enabled: bool,
    pub default_sample_interval: Option<u64>,
    pub max_events_per_stream: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotConfig {
    pub enabled: bool,
    pub base_dir: Option<String>,
    pub ffmpeg_path: Option<String>,
    pub wait_keyframe_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamConfig {
    pub id: String,
    #[serde(default)]
    pub tracks: Vec<TrackConfig>,
    pub source: Option<String>,
    pub protocol: Option<String>,
    pub pull_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrackConfig {
    pub codec: String,
    pub payload_type: u8,
    pub clock_rate: u32,
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

impl Default for Config {
    fn default() -> Self {
        Self {
            rtmp: RtmpConfig { port: 1935 },
            rtsp: RtspConfig { port: 554 },
            webrtc: WebrtcConfig { port: 9080 },
            http: HttpConfig { port: 8081 },
            hls: Some(HlsConfig {
                enabled: true,
                segment_duration: Some(1.0),
                max_segments: Some(1),
                output_dir: Some("./hls".to_string()),
            }),
            http_flv: Some(HttpFlvConfig { enabled: true }),
            record: Some(RecordConfig {
                enabled: false,
                base_dir: Some("./recordings".to_string()),
                default_format: Some("ts".to_string()),
                segment_duration_sec: Some(300),
                align_keyframe: Some(true),
            }),
            analysis: Some(AnalysisConfig {
                enabled: false,
                default_sample_interval: Some(1),
                max_events_per_stream: Some(256),
            }),
            snapshot: Some(SnapshotConfig {
                enabled: false,
                base_dir: Some("./snapshots".to_string()),
                ffmpeg_path: Some("ffmpeg".to_string()),
                wait_keyframe_ms: Some(1_000),
            }),
            streams: vec![StreamConfig {
                id: "live".to_string(),
                tracks: vec![
                    TrackConfig {
                        codec: "H264".to_string(),
                        payload_type: 96,
                        clock_rate: 90000,
                    },
                    TrackConfig {
                        codec: "AAC".to_string(),
                        payload_type: 97,
                        clock_rate: 44100,
                    },
                ],
                source: None,
                protocol: None,
                pull_url: None,
            }],
            log: LogConfig::default(),
        }
    }
}
