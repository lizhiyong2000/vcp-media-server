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

    /// Returns the number of registered pushers (for testing).
    #[cfg(test)]
    pub async fn pusher_count(&self) -> usize {
        self.pushers.read().await.len()
    }

    /// Returns the number of registered pullers (for testing).
    #[cfg(test)]
    pub async fn puller_count(&self) -> usize {
        self.pullers.read().await.len()
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

// ---------------------------------------------------------------------------
// PushPullManager tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::transport::TransportStatus;
    use std::sync::Arc;

    // ---- Pusher lifecycle -------------------------------------------------

    #[tokio::test]
    async fn register_pusher_adds_to_list() {
        let mgr = PushPullManager::new();
        assert_eq!(mgr.pusher_count().await, 0);

        mgr.register_pusher("p1".into(), "stream-a", StreamProtocol::RTMP, "rtmp://src/live")
            .await;

        assert_eq!(mgr.pusher_count().await, 1);
        let list = mgr.list_pushers().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "p1");
        assert_eq!(list[0].stream_id, "stream-a");
        assert_eq!(list[0].protocol, StreamProtocol::RTMP);
        assert_eq!(list[0].remote_url, "rtmp://src/live");
        assert_eq!(list[0].status, TransportStatus::Idle);
    }

    #[tokio::test]
    async fn unregister_pusher_removes_and_is_idempotent() {
        let mgr = PushPullManager::new();
        mgr.register_pusher("p1".into(), "s", StreamProtocol::RTMP, "url").await;
        assert_eq!(mgr.pusher_count().await, 1);

        mgr.unregister_pusher(&"p1".to_string()).await;
        assert_eq!(mgr.pusher_count().await, 0);

        // Second unregister is a no-op (no panic)
        mgr.unregister_pusher(&"p1".to_string()).await;
        assert_eq!(mgr.pusher_count().await, 0);
    }

    #[tokio::test]
    async fn update_pusher_status_transitions_between_proper_states() {
        let mgr = PushPullManager::new();
        mgr.register_pusher("p1".into(), "s", StreamProtocol::RTMP, "url").await;

        mgr.update_pusher_status(&"p1".to_string(), TransportStatus::Connecting).await;
        let info = mgr.get_pusher_info(&"p1".to_string()).await.unwrap();
        assert_eq!(info.status, TransportStatus::Connecting);

        mgr.update_pusher_status(&"p1".to_string(), TransportStatus::Publishing).await;
        let info = mgr.get_pusher_info(&"p1".to_string()).await.unwrap();
        assert_eq!(info.status, TransportStatus::Publishing);
    }

    #[tokio::test]
    async fn update_pusher_status_for_unknown_id_is_noop() {
        let mgr = PushPullManager::new();
        // Should not panic
        mgr.update_pusher_status(&"unknown".to_string(), TransportStatus::Connected).await;
    }

    #[tokio::test]
    async fn get_pusher_info_returns_none_for_unknown() {
        let mgr = PushPullManager::new();
        assert!(mgr.get_pusher_info(&"unknown".to_string()).await.is_none());
    }

    // ---- Puller lifecycle ------------------------------------------------

    #[tokio::test]
    async fn register_puller_adds_to_list() {
        let mgr = PushPullManager::new();
        assert_eq!(mgr.puller_count().await, 0);

        mgr.register_puller("pl1".into(), "stream-b", StreamProtocol::RTSP, "rtsp://src/live")
            .await;

        assert_eq!(mgr.puller_count().await, 1);
        let list = mgr.list_pullers().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "pl1");
        assert_eq!(list[0].stream_id, "stream-b");
        assert_eq!(list[0].protocol, StreamProtocol::RTSP);
        assert_eq!(list[0].remote_url, "rtsp://src/live");
        assert_eq!(list[0].status, TransportStatus::Idle);
    }

    #[tokio::test]
    async fn unregister_puller_removes_and_is_idempotent() {
        let mgr = PushPullManager::new();
        mgr.register_puller("pl1".into(), "s", StreamProtocol::RTSP, "url").await;
        assert_eq!(mgr.puller_count().await, 1);

        mgr.unregister_puller(&"pl1".to_string()).await;
        assert_eq!(mgr.puller_count().await, 0);

        mgr.unregister_puller(&"pl1".to_string()).await;
        assert_eq!(mgr.puller_count().await, 0);
    }

    #[tokio::test]
    async fn update_puller_status_transitions_between_proper_states() {
        let mgr = PushPullManager::new();
        mgr.register_puller("pl1".into(), "s", StreamProtocol::RTSP, "url").await;

        mgr.update_puller_status(&"pl1".to_string(), TransportStatus::Connecting).await;
        let info = mgr.get_puller_info(&"pl1".to_string()).await.unwrap();
        assert_eq!(info.status, TransportStatus::Connecting);

        mgr.update_puller_status(&"pl1".to_string(), TransportStatus::Playing).await;
        let info = mgr.get_puller_info(&"pl1".to_string()).await.unwrap();
        assert_eq!(info.status, TransportStatus::Playing);
    }

    #[tokio::test]
    async fn update_puller_status_for_unknown_id_is_noop() {
        let mgr = PushPullManager::new();
        mgr.update_puller_status(&"unknown".to_string(), TransportStatus::Connected).await;
    }

    #[tokio::test]
    async fn get_puller_info_returns_none_for_unknown() {
        let mgr = PushPullManager::new();
        assert!(mgr.get_puller_info(&"unknown".to_string()).await.is_none());
    }

    // ---- Concurrent operations -------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_pusher_registration_from_multiple_tasks() {
        let mgr = Arc::new(PushPullManager::new());
        const N: usize = 32;

        let mut handles = Vec::new();
        for i in 0..N {
            let mgr = Arc::clone(&mgr);
            handles.push(tokio::spawn(async move {
                mgr.register_pusher(
                    format!("p{i}"),
                    &format!("s{i}"),
                    StreamProtocol::RTMP,
                    &format!("rtmp://edge/ch{i}"),
                )
                .await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(mgr.pusher_count().await, N);
        let list = mgr.list_pushers().await;
        assert_eq!(list.len(), N);

        // Verify all ids are present
        let ids: std::collections::HashSet<_> = list.iter().map(|p| p.id.clone()).collect();
        for i in 0..N {
            assert!(ids.contains(&format!("p{i}")), "p{i} should be registered");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_status_updates_do_not_deadlock() {
        let mgr = Arc::new(PushPullManager::new());
        const N: usize = 16;

        // Register several pushers and pullers
        for i in 0..N {
            mgr.register_pusher(format!("p{i}"), &format!("s{i}"), StreamProtocol::RTMP, "u").await;
            mgr.register_puller(format!("pl{i}"), &format!("t{i}"), StreamProtocol::RTSP, "u").await;
        }

        let mut handles = Vec::new();
        let mgr = Arc::clone(&mgr);
        for i in 0..N {
            let mgr = Arc::clone(&mgr);
            handles.push(tokio::spawn(async move {
                mgr.update_pusher_status(&format!("p{i}"), TransportStatus::Publishing).await;
                mgr.update_puller_status(&format!("pl{i}"), TransportStatus::Playing).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // Verify all states updated
        for i in 0..N {
            let info = mgr.get_pusher_info(&format!("p{i}")).await.unwrap();
            assert_eq!(info.status, TransportStatus::Publishing);
            let info = mgr.get_puller_info(&format!("pl{i}")).await.unwrap();
            assert_eq!(info.status, TransportStatus::Playing);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_register_and_unregister_is_consistent() {
        let mgr = Arc::new(PushPullManager::new());
        const N: usize = 20;

        // Register N pushers
        for i in 0..N {
            mgr.register_pusher(format!("p{i}"), "s", StreamProtocol::RTMP, "u").await;
        }
        assert_eq!(mgr.pusher_count().await, N);

        // Unregister half from one task, half from another concurrently
        let mgr1 = Arc::clone(&mgr);
        let mgr2 = Arc::clone(&mgr);
        let mid = N / 2;

        let t1 = tokio::spawn(async move {
            for i in 0..mid {
                mgr1.unregister_pusher(&format!("p{i}")).await;
            }
        });
        let t2 = tokio::spawn(async move {
            for i in mid..N {
                mgr2.unregister_pusher(&format!("p{i}")).await;
            }
        });

        t1.await.unwrap();
        t2.await.unwrap();

        assert_eq!(mgr.pusher_count().await, 0);
    }

    // ---- PusherConfig and PullerConfig -----------------------------------

    #[test]
    fn pusher_config_new_stores_fields_correctly() {
        let config = PusherConfig::new("live/test", "rtmp://edge/live", StreamProtocol::RTMP);
        assert_eq!(config.stream_id, "live/test");
        assert_eq!(config.remote_url, "rtmp://edge/live");
        assert_eq!(config.protocol, StreamProtocol::RTMP);
    }

    #[test]
    fn puller_config_new_stores_fields_correctly() {
        let config = PullerConfig::new("live/test2", "rtsp://edge/live", StreamProtocol::RTSP);
        assert_eq!(config.stream_id, "live/test2");
        assert_eq!(config.remote_url, "rtsp://edge/live");
        assert_eq!(config.protocol, StreamProtocol::RTSP);
    }
}
