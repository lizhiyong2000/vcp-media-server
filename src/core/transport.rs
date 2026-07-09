use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

use super::{StreamManager, StreamProtocol};

#[derive(Debug, Clone, PartialEq)]
pub enum TransportStatus {
    Idle,
    Connecting,
    Connected,
    Publishing,
    Playing,
    Paused,
    Error(String),
    Closed,
}

impl TransportStatus {
    pub fn as_str(&self) -> &str {
        match self {
            TransportStatus::Idle => "idle",
            TransportStatus::Connecting => "connecting",
            TransportStatus::Connected => "connected",
            TransportStatus::Publishing => "publishing",
            TransportStatus::Playing => "playing",
            TransportStatus::Paused => "paused",
            TransportStatus::Error(_) => "error",
            TransportStatus::Closed => "closed",
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            TransportStatus::Connected | TransportStatus::Publishing | TransportStatus::Playing
        )
    }
}

#[async_trait]
pub trait MediaTransport: Send + Sync + 'static {
    fn protocol(&self) -> StreamProtocol;
    fn status(&self) -> TransportStatus;
    fn peer_addr(&self) -> String;
    fn stream_id(&self) -> String;

    async fn start(&mut self) -> Result<()>;
    async fn stop(&mut self) -> Result<()>;
}

#[async_trait]
pub trait StreamPuller: MediaTransport {
    async fn pull(&mut self) -> Result<()>;
}

pub type PusherId = String;

#[async_trait]
pub trait StreamPusher: MediaTransport {
    fn id(&self) -> &PusherId;
    fn remote_url(&self) -> &str;

    async fn pause(&mut self) -> Result<()>;
    async fn resume(&mut self) -> Result<()>;
}

pub struct TransportConfig {
    pub stream_manager: Arc<StreamManager>,
    pub protocol: StreamProtocol,
    pub remote_url: Option<String>,
    pub stream_id: String,
}

impl TransportConfig {
    pub fn new(
        stream_manager: Arc<StreamManager>,
        protocol: StreamProtocol,
        stream_id: &str,
    ) -> Self {
        Self {
            stream_manager,
            protocol,
            remote_url: None,
            stream_id: stream_id.to_string(),
        }
    }

    pub fn with_remote_url(mut self, remote_url: &str) -> Self {
        self.remote_url = Some(remote_url.to_string());
        self
    }
}
