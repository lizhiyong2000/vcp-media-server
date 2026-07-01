use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub rtmp: RtmpConfig,
    pub rtsp: RtspConfig,
    pub webrtc: WebrtcConfig,
    pub http: HttpConfig,
    pub hls: Option<HlsConfig>,
    pub http_flv: Option<HttpFlvConfig>,
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
            streams: vec![
                StreamConfig {
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
                },
            ],
            log: LogConfig::default(),
        }
    }
}
