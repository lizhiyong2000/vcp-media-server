use crate::core::StreamProtocol;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

pub type PusherId = String;

#[derive(Debug, Clone, PartialEq)]
pub enum PusherStatus {
    Idle,
    Starting,
    Running,
    Paused,
    Error(String),
    Stopped,
}

impl PusherStatus {
    pub fn as_str(&self) -> &str {
        match self {
            PusherStatus::Idle => "idle",
            PusherStatus::Starting => "starting",
            PusherStatus::Running => "running",
            PusherStatus::Paused => "paused",
            PusherStatus::Error(_) => "error",
            PusherStatus::Stopped => "stopped",
        }
    }

    pub fn description(&self) -> String {
        match self {
            PusherStatus::Idle => "Not started".to_string(),
            PusherStatus::Starting => "Starting".to_string(),
            PusherStatus::Running => "Running".to_string(),
            PusherStatus::Paused => "Paused".to_string(),
            PusherStatus::Error(e) => format!("Error: {}", e),
            PusherStatus::Stopped => "Stopped".to_string(),
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, PusherStatus::Running)
    }

    pub fn is_paused(&self) -> bool {
        matches!(self, PusherStatus::Paused)
    }

    pub fn is_terminated(&self) -> bool {
        matches!(self, PusherStatus::Stopped | PusherStatus::Error(_))
    }
}

pub trait StreamPusher: Send + Sync {
    fn id(&self) -> &PusherId;
    fn stream_id(&self) -> &str;
    fn protocol(&self) -> StreamProtocol;
    fn remote_url(&self) -> &str;
    fn status(&self) -> PusherStatus;

    async fn start(&mut self) -> Result<()>;
    async fn pause(&mut self) -> Result<()>;
    async fn resume(&mut self) -> Result<()>;
    async fn stop(&mut self) -> Result<()>;
}

pub struct PusherInfo {
    pub id: PusherId,
    pub stream_id: String,
    pub protocol: StreamProtocol,
    pub remote_url: String,
    pub status: PusherStatus,
}

pub struct PusherConfig {
    pub stream_id: String,
    pub remote_url: String,
    pub protocol: StreamProtocol,
}

impl PusherConfig {
    pub fn new(stream_id: &str, remote_url: &str, protocol: StreamProtocol) -> Self {
        Self {
            stream_id: stream_id.to_string(),
            remote_url: remote_url.to_string(),
            protocol,
        }
    }
}

pub struct PusherState {
    pub id: PusherId,
    pub stream_id: String,
    pub protocol: StreamProtocol,
    pub remote_url: String,
    pub status: RwLock<PusherStatus>,
}

pub struct PusherManager {
    pushers: RwLock<HashMap<PusherId, Arc<PusherState>>>,
}

impl PusherManager {
    pub fn new() -> Self {
        Self {
            pushers: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register_pusher(
        &self,
        pusher_id: PusherId,
        stream_id: &str,
        protocol: StreamProtocol,
        remote_url: &str,
    ) {
        let protocol_clone = protocol.clone();
        let pusher_id_clone = pusher_id.clone();
        let state = Arc::new(PusherState {
            id: pusher_id_clone.clone(),
            stream_id: stream_id.to_string(),
            protocol,
            remote_url: remote_url.to_string(),
            status: RwLock::new(PusherStatus::Idle),
        });
        let mut pushers = self.pushers.write().await;
        pushers.insert(pusher_id, state);
        info!(
            "[Pusher Manager] Registered pusher: id={}, stream_id={}, protocol={:?}",
            pusher_id_clone, stream_id, protocol_clone
        );
    }

    pub async fn unregister_pusher(&self, pusher_id: &PusherId) {
        let mut pushers = self.pushers.write().await;
        if pushers.remove(pusher_id).is_some() {
            info!("[Pusher Manager] Unregistered pusher: id={}", pusher_id);
        }
    }

    pub async fn update_status(&self, pusher_id: &PusherId, status: PusherStatus) {
        let pushers = self.pushers.read().await;
        if let Some(state) = pushers.get(pusher_id) {
            let mut s = state.status.write().await;
            info!(
                "[Pusher Manager] Pusher {} status changed: {} -> {}",
                pusher_id,
                s.as_str(),
                status.as_str()
            );
            *s = status;
        }
    }

    pub async fn get_pusher_info(&self, pusher_id: &PusherId) -> Option<PusherInfo> {
        let pushers = self.pushers.read().await;
        let state = pushers.get(pusher_id)?;
        let status = state.status.read().await.clone();
        Some(PusherInfo {
            id: state.id.clone(),
            stream_id: state.stream_id.clone(),
            protocol: state.protocol.clone(),
            remote_url: state.remote_url.clone(),
            status,
        })
    }

    pub async fn list_pushers(&self) -> Vec<PusherInfo> {
        let pushers = self.pushers.read().await;
        let mut result = Vec::new();
        for state in pushers.values() {
            let status = state.status.read().await.clone();
            result.push(PusherInfo {
                id: state.id.clone(),
                stream_id: state.stream_id.clone(),
                protocol: state.protocol.clone(),
                remote_url: state.remote_url.clone(),
                status,
            });
        }
        result
    }
}
