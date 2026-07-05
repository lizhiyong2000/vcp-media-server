use crate::core::StreamProtocol;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use super::transport::{PusherId, TransportStatus};

pub type PullerId = String;

pub struct PusherInfo {
    pub id: PusherId,
    pub stream_id: String,
    pub protocol: StreamProtocol,
    pub remote_url: String,
    pub status: TransportStatus,
}

pub struct PullerInfo {
    pub id: PullerId,
    pub stream_id: String,
    pub protocol: StreamProtocol,
    pub remote_url: String,
    pub status: TransportStatus,
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

pub struct PullerConfig {
    pub stream_id: String,
    pub remote_url: String,
    pub protocol: StreamProtocol,
}

impl PullerConfig {
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
    pub status: RwLock<TransportStatus>,
}

pub struct PullerState {
    pub id: PullerId,
    pub stream_id: String,
    pub protocol: StreamProtocol,
    pub remote_url: String,
    pub status: RwLock<TransportStatus>,
}

pub struct PushPullManager {
    pushers: RwLock<HashMap<PusherId, Arc<PusherState>>>,
    pullers: RwLock<HashMap<PullerId, Arc<PullerState>>>,
}

impl PushPullManager {
    pub fn new() -> Self {
        Self {
            pushers: RwLock::new(HashMap::new()),
            pullers: RwLock::new(HashMap::new()),
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
            status: RwLock::new(TransportStatus::Idle),
        });
        let mut pushers = self.pushers.write().await;
        pushers.insert(pusher_id, state);
        info!(
            "[PushPull Manager] Registered pusher: id={}, stream_id={}, protocol={:?}",
            pusher_id_clone, stream_id, protocol_clone
        );
    }

    pub async fn unregister_pusher(&self, pusher_id: &PusherId) {
        let mut pushers = self.pushers.write().await;
        if pushers.remove(pusher_id).is_some() {
            info!("[PushPull Manager] Unregistered pusher: id={}", pusher_id);
        }
    }

    pub async fn update_pusher_status(&self, pusher_id: &PusherId, status: TransportStatus) {
        let pushers = self.pushers.read().await;
        if let Some(state) = pushers.get(pusher_id) {
            let mut s = state.status.write().await;
            info!(
                "[PushPull Manager] Pusher {} status changed: {} -> {}",
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

    pub async fn register_puller(
        &self,
        puller_id: PullerId,
        stream_id: &str,
        protocol: StreamProtocol,
        remote_url: &str,
    ) {
        let protocol_clone = protocol.clone();
        let puller_id_clone = puller_id.clone();
        let state = Arc::new(PullerState {
            id: puller_id_clone.clone(),
            stream_id: stream_id.to_string(),
            protocol,
            remote_url: remote_url.to_string(),
            status: RwLock::new(TransportStatus::Idle),
        });
        let mut pullers = self.pullers.write().await;
        pullers.insert(puller_id, state);
        info!(
            "[PushPull Manager] Registered puller: id={}, stream_id={}, protocol={:?}",
            puller_id_clone, stream_id, protocol_clone
        );
    }

    pub async fn unregister_puller(&self, puller_id: &PullerId) {
        let mut pullers = self.pullers.write().await;
        if pullers.remove(puller_id).is_some() {
            info!("[PushPull Manager] Unregistered puller: id={}", puller_id);
        }
    }

    pub async fn update_puller_status(&self, puller_id: &PullerId, status: TransportStatus) {
        let pullers = self.pullers.read().await;
        if let Some(state) = pullers.get(puller_id) {
            let mut s = state.status.write().await;
            info!(
                "[PushPull Manager] Puller {} status changed: {} -> {}",
                puller_id,
                s.as_str(),
                status.as_str()
            );
            *s = status;
        }
    }

    pub async fn get_puller_info(&self, puller_id: &PullerId) -> Option<PullerInfo> {
        let pullers = self.pullers.read().await;
        let state = pullers.get(puller_id)?;
        let status = state.status.read().await.clone();
        Some(PullerInfo {
            id: state.id.clone(),
            stream_id: state.stream_id.clone(),
            protocol: state.protocol.clone(),
            remote_url: state.remote_url.clone(),
            status,
        })
    }

    pub async fn list_pullers(&self) -> Vec<PullerInfo> {
        let pullers = self.pullers.read().await;
        let mut result = Vec::new();
        for state in pullers.values() {
            let status = state.status.read().await.clone();
            result.push(PullerInfo {
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
