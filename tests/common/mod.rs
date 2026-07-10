//! Common test utilities for integration tests.
//!
//! This module provides shared test infrastructure for cross-protocol streaming tests.

pub mod ffmpeg_helper;
pub mod frame_generators;
pub mod hls_helper;
pub mod http_client;
pub mod http_flv_helper;
pub mod media_server_runner;
pub mod process_manager;
pub mod rtmp_helper;
pub mod rtsp_helper;
pub mod server_helper;
pub mod stream_helper;
pub mod verify;

/// Test configuration for cross-protocol tests.
#[derive(Debug, Clone)]
pub struct TestConfig {
    pub stream_id: String,
    pub codec: TestCodec,
    pub duration_secs: u64,
}

impl TestConfig {
    pub fn new(stream_id: &str, codec: TestCodec) -> Self {
        Self {
            stream_id: stream_id.to_string(),
            codec,
            duration_secs: 5,
        }
    }

    pub fn with_duration(mut self, duration: u64) -> Self {
        self.duration_secs = duration;
        self
    }
}

/// Supported test codecs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestCodec {
    H264,
    H265,
}

impl TestCodec {
    pub fn as_str(&self) -> &'static str {
        match self {
            TestCodec::H264 => "H264",
            TestCodec::H265 => "H265",
        }
    }

    pub fn payload_type(&self) -> u8 {
        match self {
            TestCodec::H264 => 96,
            TestCodec::H265 => 98,
        }
    }
}

/// Default server ports for testing.
pub mod ports {
    pub const RTMP_PORT: u16 = 1935;
    pub const RTSP_PORT: u16 = 554;
    pub const HTTP_PORT: u16 = 8081;
    pub const WEBRTC_PORT: u16 = 9080;

    /// Get a random unused port for testing.
    pub fn random_test_port() -> u16 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        ((10000 + (seed % 50000)) as u16).max(10000)
    }
}

/// Default test timeouts.
pub mod timeouts {
    use std::time::Duration;

    pub const FRAME_WAIT: Duration = Duration::from_millis(100);
    pub const STREAM_WAIT: Duration = Duration::from_millis(500);
    pub const SERVER_START: Duration = Duration::from_secs(5);
    pub const CLEANUP_WAIT: Duration = Duration::from_millis(100);
}
