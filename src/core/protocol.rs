/// Protocol abstraction layer
/// Provides common traits for converting media frames to protocol-specific formats.
/// Each protocol (RTSP, RTMP, HLS, HTTP-FLV) implements these traits for unified frame dispatch.
use anyhow::Result;
use async_trait::async_trait;

use crate::core::{MediaFrame, Stream};

/// Trait for converting media frames to protocol-specific output format
#[async_trait]
pub trait StreamSink: Send + Sync {
    /// Convert a media frame to protocol-specific byte format
    async fn on_frame(&mut self, frame: &MediaFrame) -> Result<Vec<u8>>;
    /// Generate protocol-specific stream header/initial data
    async fn generate_header(&self, stream: &Stream) -> Result<Vec<u8>>;
}

/// Trait for protocol-specific stream source (push/pull)
#[async_trait]
pub trait StreamSource: Send + Sync {
    /// Read the next frame from the source
    async fn read_frame(&mut self) -> Result<Option<MediaFrame>>;
    /// Whether the source is still active
    fn is_active(&self) -> bool;
}

/// Supported protocol types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolType {
    RTSP,
    RTMP,
    HLS,
    HTTP_FLV,
    WebRTC,
    HTTP_API,
}

impl ProtocolType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProtocolType::RTSP => "RTSP",
            ProtocolType::RTMP => "RTMP",
            ProtocolType::HLS => "HLS",
            ProtocolType::HTTP_FLV => "HTTP-FLV",
            ProtocolType::WebRTC => "WebRTC",
            ProtocolType::HTTP_API => "HTTP-API",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "RTSP" => Some(ProtocolType::RTSP),
            "RTMP" => Some(ProtocolType::RTMP),
            "HLS" => Some(ProtocolType::HLS),
            "HTTP-FLV" | "HTTPFLV" | "FLV" => Some(ProtocolType::HTTP_FLV),
            "WEBRTC" => Some(ProtocolType::WebRTC),
            "HTTP" | "HTTP-API" => Some(ProtocolType::HTTP_API),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProtocolType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Information about a protocol handler
#[derive(Debug, Clone)]
pub struct ProtocolInfo {
    pub protocol: ProtocolType,
    pub enabled: bool,
    pub port: u16,
    pub description: String,
}

/// Registry of active protocol handlers
pub struct ProtocolRegistry {
    protocols: Vec<ProtocolInfo>,
}

impl ProtocolRegistry {
    pub fn new() -> Self {
        Self {
            protocols: Vec::new(),
        }
    }

    pub fn register(&mut self, info: ProtocolInfo) {
        self.protocols.push(info);
    }

    pub fn list(&self) -> &[ProtocolInfo] {
        &self.protocols
    }

    pub fn is_enabled(&self, protocol: ProtocolType) -> bool {
        self.protocols.iter().any(|p| p.protocol == protocol && p.enabled)
    }
}

impl Default for ProtocolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
