//! Central registry for streams, frame hubs, and playback receivers.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use parking_lot::RwLock;
use tracing::{debug, info, warn};

use super::dispatch::{DispatchPolicy, DispatchReader};
use super::stream_hub::StreamHub;
use super::{
    CodecType, MediaFrame, PlaybackStatus, ReceiverId, ReceiverStatus, Stream, StreamId,
    StreamProtocol, StreamReceiver, StreamSinkMode, StreamSourceMode, StreamStatus, Track,
};

pub struct StreamManager {
    hubs: RwLock<HashMap<StreamId, Arc<StreamHub>>>,
}

impl StreamManager {
    pub fn new() -> Self {
        Self {
            hubs: RwLock::new(HashMap::new()),
        }
    }

    pub fn create_stream(
        &self,
        stream_id: &str,
        source: StreamSourceMode,
        protocol: StreamProtocol,
        pull_url: Option<String>,
    ) -> Stream {
        if let Some(existing_hub) = self.get_hub(stream_id) {
            info!(
                "[Core] Stream {} already exists, returning existing stream",
                stream_id
            );
            return existing_hub.stream();
        }

        let stream = Stream {
            id: stream_id.to_string(),
            tracks: Vec::new(),
            status: StreamStatus::Created,
            playback_status: PlaybackStatus::Idle,
            source,
            protocol,
            pull_url,
            sps: None,
            pps: None,
        };

        let mut hubs = self.hubs.write();
        if let Some(existing_hub) = hubs.get(stream_id) {
            info!(
                "[Core] Stream {} already exists, returning existing stream",
                stream_id
            );
            return existing_hub.stream();
        }
        let hub = StreamHub::with_stream(stream.clone());
        hubs.insert(stream_id.to_string(), hub);
        info!("[Core] StreamHub ready for stream '{}'", stream_id);

        stream
    }

    pub fn set_stream_tracks(&self, stream_id: &str, tracks: Vec<Track>) {
        if let Some(hub) = self.get_hub(stream_id) {
            hub.set_tracks(tracks);
        }
    }

    pub fn set_stream_sps_pps(&self, stream_id: &str, sps: Vec<u8>, pps: Vec<u8>) {
        if let Some(hub) = self.get_hub(stream_id) {
            hub.update_stream(|stream| {
                if stream.sps.is_none() {
                    info!(
                        "[Core] Setting SPS ({}) and PPS ({}) for stream {}",
                        sps.len(),
                        pps.len(),
                        stream_id
                    );
                }
                stream.sps = Some(sps);
                stream.pps = Some(pps);
            });
        }
    }

    /// Merge SPS/PPS from a single NALU into stream codec config.
    pub fn merge_stream_nalu_config(&self, stream_id: &str, nalu: &[u8]) {
        if nalu.is_empty() {
            return;
        }
        let nal_type = nalu[0] & 0x1F;
        if nal_type != 7 && nal_type != 8 {
            return;
        }
        if let Some(hub) = self.get_hub(stream_id) {
            hub.update_stream(|stream| {
                match nal_type {
                    7 if stream.sps.is_none() => stream.sps = Some(nalu.to_vec()),
                    8 if stream.pps.is_none() => stream.pps = Some(nalu.to_vec()),
                    _ => {}
                }
                if let (Some(sps), Some(pps)) = (&stream.sps, &stream.pps) {
                    info!(
                        "[Core] Stream {} codec config ready (sps={} pps={})",
                        stream_id,
                        sps.len(),
                        pps.len()
                    );
                }
            });
        }
    }

    pub fn ensure_stream_hub(&self, stream_id: &str) {
        if self.get_hub(stream_id).is_some() {
            debug!("[Core] StreamHub already exists for stream '{}'", stream_id);
            return;
        }
        let _ = self.get_or_create_hub(stream_id);
    }

    /// Back-compat alias.
    pub fn ensure_stream_broadcast(&self, stream_id: &str) {
        self.ensure_stream_hub(stream_id);
    }

    pub fn set_stream_hub(&self, stream_id: &str) {
        let _ = self.get_or_create_hub(stream_id);
    }

    /// Back-compat alias.
    pub fn set_stream_broadcast(&self, stream_id: &str) {
        self.set_stream_hub(stream_id);
    }

    pub fn get_hub(&self, stream_id: &str) -> Option<Arc<StreamHub>> {
        self.hubs.read().get(stream_id).cloned()
    }

    fn get_or_create_hub(&self, stream_id: &str) -> Option<Arc<StreamHub>> {
        self.get_hub(stream_id)
    }

    pub fn remove_stream(&self, stream_id: &StreamId) -> Option<Stream> {
        self.hubs.write().remove(stream_id).map(|hub| hub.stream())
    }

    pub fn get_stream(&self, stream_id: &StreamId) -> Option<Stream> {
        self.get_hub(stream_id).map(|hub| hub.stream())
    }

    pub fn list_streams(&self) -> Vec<StreamId> {
        let hubs = self.hubs.read();
        hubs.keys().cloned().collect()
    }

    pub fn set_status(&self, stream_id: &str, status: StreamStatus) -> Result<()> {
        if let Some(hub) = self.get_hub(stream_id) {
            hub.update_stream(|stream| {
                let old_status = stream.status.clone();
                stream.status = status.clone();
                info!(
                    "[Core] Stream {} status changed from {:?} to {:?}",
                    stream_id, old_status, status
                );
            });
            Ok(())
        } else {
            Err(anyhow::anyhow!("Stream {} not found", stream_id))
        }
    }

    pub fn set_playback_status(&self, stream_id: &str, status: PlaybackStatus) -> Result<()> {
        if let Some(hub) = self.get_hub(stream_id) {
            hub.update_stream(|stream| {
                let old_status = stream.playback_status.clone();
                stream.playback_status = status.clone();
                info!(
                    "[Core] Stream {} playback status changed from {:?} to {:?}",
                    stream_id, old_status, status
                );
            });
            Ok(())
        } else {
            Err(anyhow::anyhow!("Stream {} not found", stream_id))
        }
    }

    pub fn set_created(&self, stream_id: &str) -> Result<()> {
        self.set_status(stream_id, StreamStatus::Created)
    }

    pub fn set_unpublished(&self, stream_id: &str) -> Result<()> {
        self.set_status(stream_id, StreamStatus::Unpublished)
    }

    pub fn set_publishing(&self, stream_id: &str) -> Result<()> {
        self.set_status(stream_id, StreamStatus::Publishing)
    }

    pub fn set_paused(&self, stream_id: &str) -> Result<()> {
        self.set_status(stream_id, StreamStatus::Paused)
    }

    pub fn set_error(&self, stream_id: &str, error: &str) -> Result<()> {
        self.set_status(stream_id, StreamStatus::Error(error.to_string()))
    }

    pub fn set_playback_idle(&self, stream_id: &str) -> Result<()> {
        self.set_playback_status(stream_id, PlaybackStatus::Idle)
    }

    pub fn set_playback_playing(&self, stream_id: &str) -> Result<()> {
        self.set_playback_status(stream_id, PlaybackStatus::Playing)
    }

    pub fn set_playback_paused(&self, stream_id: &str) -> Result<()> {
        self.set_playback_status(stream_id, PlaybackStatus::Paused)
    }

    pub fn set_stopped(&self, stream_id: &str) -> Result<()> {
        self.set_status(stream_id, StreamStatus::Stopped)
    }

    pub fn acquire_publisher(&self, stream_id: &str, publisher_id: &str) -> Result<()> {
        let hub = self
            .get_hub(stream_id)
            .ok_or_else(|| anyhow::anyhow!("Stream {} not found", stream_id))?;

        match hub.acquire_publisher(publisher_id) {
            Ok(()) => {
                info!(
                    "[Core] Publisher {} acquired stream {}",
                    publisher_id, stream_id
                );
                Ok(())
            }
            Err(current) => {
                warn!(
                    "[Core] Rejecting publisher {} for stream {}, already held by {}",
                    publisher_id, stream_id, current
                );
                Err(anyhow::anyhow!(
                    "Stream {} already has an active publisher",
                    stream_id
                ))
            }
        }
    }

    pub fn release_publisher(&self, stream_id: &str, publisher_id: &str) -> bool {
        let released = self
            .get_hub(stream_id)
            .map(|hub| hub.release_publisher(publisher_id))
            .unwrap_or(false);

        if released {
            info!(
                "[Core] Publisher {} released stream {}",
                publisher_id, stream_id
            );
        } else {
            debug!(
                "[Core] Publisher {} did not own stream {}, release ignored",
                publisher_id, stream_id
            );
        }

        released
    }

    pub fn current_publisher(&self, stream_id: &str) -> Option<String> {
        self.get_hub(stream_id)
            .and_then(|hub| hub.current_publisher())
    }

    pub fn create_receiver(
        &self,
        stream_id: &str,
        mode: StreamSinkMode,
        protocol: StreamProtocol,
    ) -> StreamReceiver {
        let receiver = StreamReceiver::new(stream_id, mode, protocol);

        if let Some(hub) = self.get_hub(stream_id) {
            hub.add_receiver(receiver.clone());
        } else {
            warn!(
                "[Core] Created receiver {} for missing stream {}, receiver is not registered",
                receiver.id, stream_id
            );
        }

        info!(
            "[Core] Created receiver {} for stream {}",
            receiver.id, stream_id
        );
        receiver
    }

    pub fn create_pull_receiver(
        &self,
        stream_id: &str,
        protocol: StreamProtocol,
        client_addr: &str,
    ) -> StreamReceiver {
        let receiver = StreamReceiver::new(stream_id, StreamSinkMode::Pull, protocol)
            .with_client_addr(client_addr);

        if let Some(hub) = self.get_hub(stream_id) {
            hub.add_receiver(receiver.clone());
        } else {
            warn!(
                "[Core] Created pull receiver {} for missing stream {}, receiver is not registered",
                receiver.id, stream_id
            );
        }

        info!(
            "[Core] Created pull receiver {} for stream {} from client {}",
            receiver.id, stream_id, client_addr
        );
        receiver
    }

    pub fn create_push_receiver(
        &self,
        stream_id: &str,
        protocol: StreamProtocol,
        push_addr: &str,
    ) -> StreamReceiver {
        let receiver = StreamReceiver::new(stream_id, StreamSinkMode::Push, protocol)
            .with_push_addr(push_addr);

        if let Some(hub) = self.get_hub(stream_id) {
            hub.add_receiver(receiver.clone());
        } else {
            warn!(
                "[Core] Created push receiver {} for missing stream {}, receiver is not registered",
                receiver.id, stream_id
            );
        }

        info!(
            "[Core] Created push receiver {} for stream {} to {}",
            receiver.id, stream_id, push_addr
        );
        receiver
    }

    pub fn remove_receiver(&self, receiver_id: &ReceiverId) -> Option<StreamReceiver> {
        let receiver = self
            .hubs
            .read()
            .values()
            .find_map(|hub| hub.remove_receiver(receiver_id));

        if let Some(r) = &receiver {
            info!(
                "[Core] Removed receiver {} for stream {}",
                r.id, r.stream_id
            );
        }

        receiver
    }

    pub fn get_receiver(&self, receiver_id: &ReceiverId) -> Option<StreamReceiver> {
        self.hubs
            .read()
            .values()
            .find_map(|hub| hub.get_receiver(receiver_id))
    }

    pub fn list_receivers(&self) -> Vec<ReceiverId> {
        self.hubs
            .read()
            .values()
            .flat_map(|hub| hub.list_receiver_ids())
            .collect()
    }

    pub fn list_receivers_for_stream(&self, stream_id: &StreamId) -> Vec<StreamReceiver> {
        self.get_hub(stream_id)
            .map(|hub| hub.list_receivers())
            .unwrap_or_default()
    }

    pub fn set_receiver_status(
        &self,
        receiver_id: &ReceiverId,
        status: ReceiverStatus,
    ) -> Result<()> {
        let old_status = self
            .hubs
            .read()
            .values()
            .find_map(|hub| hub.set_receiver_status(receiver_id, status.clone()));
        if let Some(old_status) = old_status {
            info!(
                "[Core] Receiver {} status changed from {:?} to {:?}",
                receiver_id, old_status, status
            );
            Ok(())
        } else {
            Err(anyhow::anyhow!("Receiver {} not found", receiver_id))
        }
    }

    pub fn get_last_keyframe(&self, stream_id: &str) -> Option<(Vec<u8>, u64)> {
        self.get_hub(stream_id)?.latest_idr_bytes()
    }

    pub fn dispatch_subscribe(
        &self,
        stream_id: &str,
        policy: DispatchPolicy,
    ) -> Option<DispatchReader> {
        let hub = self.get_or_create_hub(stream_id)?;
        info!(
            "[Core] dispatch_subscribe: stream_id={} policy={:?}",
            stream_id, policy
        );
        Some(DispatchReader::new(hub, policy))
    }

    pub fn publish_frame(&self, frame: MediaFrame) {
        let stream_id = frame.stream_id.clone();
        debug!(
            "[Core] publish_frame: stream_id={}, track_id={}, timestamp={}, is_keyframe={}, codec={}, data_len={}",
            stream_id,
            frame.track_id,
            frame.timestamp,
            frame.is_keyframe,
            frame.codec as u8,
            frame.data.len()
        );

        if frame.is_keyframe && matches!(frame.codec, CodecType::H264 | CodecType::H265) {
            debug!(
                "[Core] Keyframe stream='{}' ts={}",
                stream_id, frame.timestamp
            );
        }

        if let Some(hub) = self.get_or_create_hub(&stream_id) {
            let seq = hub.publish(frame);
            debug!(
                "[Core] publish_frame: stream_id={} ring_seq={}",
                stream_id, seq
            );
        } else {
            warn!(
                "[Core] publish_frame: No StreamHub for stream_id={}",
                stream_id
            );
        }

        if let Some(hub) = self.get_hub(&stream_id) {
            hub.update_stream(|stream| match stream.status {
                StreamStatus::Created | StreamStatus::Unpublished => {
                    stream.status = StreamStatus::Publishing;
                    debug!("[Core] Stream {} status -> Publishing", stream_id);
                }
                _ => {}
            });
        }
    }

    pub fn update_stream_status(&self, stream_id: &StreamId, status: StreamStatus) {
        if let Some(hub) = self.get_hub(stream_id) {
            hub.update_stream(|stream| {
                stream.status = status;
            });
        }
    }
}

impl Default for StreamManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::collections::{HashMap, HashSet};

    fn create_test_stream(manager: &StreamManager, stream_id: &str) -> Stream {
        manager.create_stream(
            stream_id,
            StreamSourceMode::Pull,
            StreamProtocol::RTSP,
            Some(format!("rtsp://example/{stream_id}")),
        )
    }

    fn h264_frame(stream_id: &str, timestamp: u64, keyframe: bool) -> MediaFrame {
        let nal = if keyframe { 0x65 } else { 0x41 };
        MediaFrame::new(
            stream_id.to_string(),
            0,
            timestamp,
            Bytes::from(vec![0, 0, 0, 1, nal]),
            keyframe,
            CodecType::H264,
        )
    }

    #[test]
    fn create_stream_registers_single_hub_that_holds_stream() {
        let manager = StreamManager::new();

        let stream = create_test_stream(&manager, "s");
        let hub = manager.get_hub("s").expect("hub should be registered");

        assert_eq!(stream.id, "s");
        assert_eq!(hub.stream().id, "s");
        assert_eq!(
            manager.get_stream(&"s".to_string()).unwrap().protocol,
            StreamProtocol::RTSP
        );
        assert_eq!(manager.list_streams(), vec!["s".to_string()]);
    }

    #[test]
    fn duplicate_create_stream_keeps_existing_hub_and_metadata() {
        let manager = StreamManager::new();

        let first = manager.create_stream(
            "s",
            StreamSourceMode::Pull,
            StreamProtocol::RTSP,
            Some("rtsp://example/original".to_string()),
        );
        let first_hub = manager.get_hub("s").expect("hub should exist");

        let second = manager.create_stream(
            "s",
            StreamSourceMode::Push,
            StreamProtocol::RTMP,
            Some("rtmp://example/replacement".to_string()),
        );
        let second_hub = manager.get_hub("s").expect("hub should still exist");

        assert!(Arc::ptr_eq(&first_hub, &second_hub));
        assert_eq!(first.source, StreamSourceMode::Pull);
        assert_eq!(second.source, StreamSourceMode::Pull);
        assert_eq!(second.protocol, StreamProtocol::RTSP);
        assert_eq!(second.pull_url.as_deref(), Some("rtsp://example/original"));
        assert_eq!(manager.list_streams().len(), 1);
    }

    #[test]
    fn ensure_or_set_hub_does_not_create_unknown_stream() {
        let manager = StreamManager::new();

        manager.ensure_stream_hub("missing");
        manager.set_stream_hub("missing");

        assert!(manager.get_hub("missing").is_none());
        assert!(manager.get_stream(&"missing".to_string()).is_none());
        assert!(manager.list_streams().is_empty());
        assert!(manager
            .dispatch_subscribe("missing", DispatchPolicy::SequentialFromIdr)
            .is_none());
    }

    #[test]
    fn set_stream_hub_does_not_replace_existing_hub() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::Unknown, None);

        manager.set_stream_hub("s");
        let first = manager.get_hub("s").expect("hub should be created");

        manager.set_stream_hub("s");
        let second = manager.get_hub("s").expect("hub should still exist");

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn stream_metadata_updates_are_applied_to_stream_inside_hub() {
        let manager = StreamManager::new();
        create_test_stream(&manager, "s");

        manager.set_stream_tracks("s", vec![Track::new(1, CodecType::AAC, 97, 44_100)]);
        manager.set_stream_sps_pps("s", vec![0x67], vec![0x68]);
        manager.set_playback_playing("s").unwrap();
        manager.set_paused("s").unwrap();

        let from_manager = manager.get_stream(&"s".to_string()).expect("stream");
        let from_hub = manager.get_hub("s").expect("hub").stream();

        assert_eq!(from_manager.tracks.len(), 1);
        assert_eq!(from_manager.tracks[0].codec, CodecType::AAC);
        assert_eq!(from_manager.sps.as_deref(), Some(&[0x67][..]));
        assert_eq!(from_manager.pps.as_deref(), Some(&[0x68][..]));
        assert_eq!(from_manager.playback_status, PlaybackStatus::Playing);
        assert_eq!(from_manager.status, StreamStatus::Paused);
        assert_eq!(from_hub.status, StreamStatus::Paused);
    }

    #[test]
    fn metadata_updates_for_missing_stream_do_not_create_hub() {
        let manager = StreamManager::new();

        manager.set_stream_tracks("missing", vec![Track::new(1, CodecType::AAC, 97, 44_100)]);
        manager.set_stream_sps_pps("missing", vec![0x67], vec![0x68]);
        manager.merge_stream_nalu_config("missing", &[0x67, 0x42]);
        manager.update_stream_status(&"missing".to_string(), StreamStatus::Publishing);

        assert!(manager.get_hub("missing").is_none());
        assert!(manager.list_streams().is_empty());
        assert!(manager
            .set_status("missing", StreamStatus::Publishing)
            .is_err());
        assert!(manager
            .set_playback_status("missing", PlaybackStatus::Playing)
            .is_err());
    }

    #[test]
    fn merge_stream_nalu_config_sets_each_config_once_and_ignores_other_nalus() {
        let manager = StreamManager::new();
        create_test_stream(&manager, "s");

        manager.merge_stream_nalu_config("s", &[]);
        manager.merge_stream_nalu_config("s", &[0x65, 0x01]);
        assert!(manager.get_stream(&"s".to_string()).unwrap().sps.is_none());
        assert!(manager.get_stream(&"s".to_string()).unwrap().pps.is_none());

        manager.merge_stream_nalu_config("s", &[0x67, 0x01]);
        manager.merge_stream_nalu_config("s", &[0x68, 0x02]);
        manager.merge_stream_nalu_config("s", &[0x67, 0xff]);
        manager.merge_stream_nalu_config("s", &[0x68, 0xee]);

        let stream = manager.get_stream(&"s".to_string()).unwrap();
        assert_eq!(stream.sps.as_deref(), Some(&[0x67, 0x01][..]));
        assert_eq!(stream.pps.as_deref(), Some(&[0x68, 0x02][..]));
    }

    #[test]
    fn publish_frame_requires_registered_stream_and_updates_expected_statuses() {
        let manager = StreamManager::new();

        manager.publish_frame(h264_frame("missing", 1000, true));
        assert!(manager.get_hub("missing").is_none());

        create_test_stream(&manager, "s");
        manager.publish_frame(h264_frame("s", 1000, true));
        assert_eq!(
            manager.get_stream(&"s".to_string()).unwrap().status,
            StreamStatus::Publishing
        );
        assert_eq!(
            manager.get_last_keyframe("s"),
            Some((vec![0, 0, 0, 1, 0x65], 1000))
        );

        manager.set_unpublished("s").unwrap();
        manager.publish_frame(h264_frame("s", 2000, false));
        assert_eq!(
            manager.get_stream(&"s".to_string()).unwrap().status,
            StreamStatus::Publishing
        );

        manager.set_paused("s").unwrap();
        manager.publish_frame(h264_frame("s", 3000, false));
        assert_eq!(
            manager.get_stream(&"s".to_string()).unwrap().status,
            StreamStatus::Paused
        );

        manager.set_error("s", "boom").unwrap();
        manager.publish_frame(h264_frame("s", 4000, false));
        assert!(matches!(
            manager.get_stream(&"s".to_string()).unwrap().status,
            StreamStatus::Error(ref err) if err == "boom"
        ));
    }

    #[test]
    fn remove_stream_returns_stream_snapshot_and_removes_hub() {
        let manager = StreamManager::new();
        create_test_stream(&manager, "s");
        manager.set_publishing("s").unwrap();
        let receiver = manager.create_receiver("s", StreamSinkMode::Pull, StreamProtocol::HTTP);
        manager.publish_frame(h264_frame("s", 1000, true));

        let removed = manager
            .remove_stream(&"s".to_string())
            .expect("stream should be removed");

        assert_eq!(removed.id, "s");
        assert_eq!(removed.status, StreamStatus::Publishing);
        assert!(manager.get_hub("s").is_none());
        assert!(manager.get_stream(&"s".to_string()).is_none());
        assert!(manager.get_receiver(&receiver.id).is_none());
        assert!(manager
            .list_receivers_for_stream(&"s".to_string())
            .is_empty());
        assert!(manager.get_last_keyframe("s").is_none());
        assert!(manager
            .dispatch_subscribe("s", DispatchPolicy::SequentialFromIdr)
            .is_none());
    }

    #[test]
    fn receiver_lifecycle_is_managed_by_stream_hub() {
        let manager = StreamManager::new();
        create_test_stream(&manager, "s1");
        create_test_stream(&manager, "s2");

        let pull = manager.create_pull_receiver("s1", StreamProtocol::RTSP, "127.0.0.1:554");
        let push = manager.create_push_receiver("s2", StreamProtocol::RTMP, "rtmp://edge/live");

        assert_eq!(pull.stream_id, "s1");
        assert_eq!(pull.client_addr.as_deref(), Some("127.0.0.1:554"));
        assert_eq!(push.stream_id, "s2");
        assert_eq!(push.push_addr.as_deref(), Some("rtmp://edge/live"));

        let ids = manager.list_receivers();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&pull.id));
        assert!(ids.contains(&push.id));

        assert_eq!(
            manager.list_receivers_for_stream(&"s1".to_string()).len(),
            1
        );
        assert_eq!(
            manager.list_receivers_for_stream(&"s2".to_string()).len(),
            1
        );
        assert_eq!(
            manager.get_hub("s1").unwrap().list_receivers()[0].id,
            pull.id
        );

        manager
            .set_receiver_status(&pull.id, ReceiverStatus::Playing)
            .unwrap();
        assert_eq!(
            manager.get_receiver(&pull.id).expect("receiver").status,
            ReceiverStatus::Playing
        );
        assert_eq!(
            manager
                .get_hub("s1")
                .unwrap()
                .get_receiver(&pull.id)
                .unwrap()
                .status,
            ReceiverStatus::Playing
        );

        let removed = manager
            .remove_receiver(&pull.id)
            .expect("receiver should be removed");
        assert_eq!(removed.id, pull.id);
        assert!(manager.get_receiver(&pull.id).is_none());
        assert!(manager
            .list_receivers_for_stream(&"s1".to_string())
            .is_empty());
        assert_eq!(
            manager.list_receivers_for_stream(&"s2".to_string()).len(),
            1
        );
    }

    #[test]
    fn receivers_for_missing_stream_are_not_registered() {
        let manager = StreamManager::new();

        let receiver =
            manager.create_receiver("missing", StreamSinkMode::Pull, StreamProtocol::HTTP);

        assert_eq!(receiver.stream_id, "missing");
        assert!(manager.get_hub("missing").is_none());
        assert!(manager.get_receiver(&receiver.id).is_none());
        assert!(manager.list_receivers().is_empty());
        assert!(manager
            .set_receiver_status(&receiver.id, ReceiverStatus::Playing)
            .is_err());
        assert!(manager.remove_receiver(&receiver.id).is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn receiver_aggregation_stays_consistent_during_concurrent_multi_stream_operations() {
        const STREAM_COUNT: usize = 4;
        const RECEIVERS_PER_STREAM: usize = 16;

        let manager = Arc::new(StreamManager::new());
        for stream_idx in 0..STREAM_COUNT {
            create_test_stream(&manager, &format!("s{stream_idx}"));
        }

        let mut tasks = Vec::new();
        for stream_idx in 0..STREAM_COUNT {
            let manager = Arc::clone(&manager);
            tasks.push(tokio::spawn(async move {
                let stream_id = format!("s{stream_idx}");
                let mut kept = Vec::new();
                let mut removed = Vec::new();

                for receiver_idx in 0..RECEIVERS_PER_STREAM {
                    let receiver = if receiver_idx % 2 == 0 {
                        manager.create_pull_receiver(
                            &stream_id,
                            StreamProtocol::RTSP,
                            &format!("127.0.0.1:{}", 10_000 + receiver_idx),
                        )
                    } else {
                        manager.create_push_receiver(
                            &stream_id,
                            StreamProtocol::RTMP,
                            &format!("rtmp://edge/{stream_id}/{receiver_idx}"),
                        )
                    };

                    let expected_status = if receiver_idx % 3 == 0 {
                        manager
                            .set_receiver_status(&receiver.id, ReceiverStatus::Playing)
                            .expect("registered receiver should accept status updates");
                        ReceiverStatus::Playing
                    } else {
                        ReceiverStatus::Idle
                    };

                    if receiver_idx % 5 == 0 {
                        let removed_receiver = manager
                            .remove_receiver(&receiver.id)
                            .expect("registered receiver should be removable");
                        removed.push(removed_receiver.id);
                    } else {
                        kept.push((receiver.id, stream_id.clone(), expected_status));
                    }

                    tokio::task::yield_now().await;
                }

                (kept, removed)
            }));
        }

        let mut expected_by_stream: HashMap<String, Vec<(ReceiverId, ReceiverStatus)>> =
            HashMap::new();
        let mut expected_ids = HashSet::new();
        let mut removed_ids = HashSet::new();

        for task in tasks {
            let (kept, removed) = task.await.expect("receiver task should finish");
            for removed_id in removed {
                assert!(removed_ids.insert(removed_id));
            }
            for (receiver_id, stream_id, status) in kept {
                assert!(expected_ids.insert(receiver_id.clone()));
                expected_by_stream
                    .entry(stream_id)
                    .or_default()
                    .push((receiver_id, status));
            }
        }

        let aggregated_ids: HashSet<_> = manager.list_receivers().into_iter().collect();
        assert_eq!(aggregated_ids, expected_ids);

        for (stream_id, expected) in expected_by_stream {
            let stream_receivers = manager.list_receivers_for_stream(&stream_id);
            let stream_ids: HashSet<_> = stream_receivers.iter().map(|r| r.id.clone()).collect();
            let expected_stream_ids: HashSet<_> = expected
                .iter()
                .map(|(receiver_id, _)| receiver_id.clone())
                .collect();

            assert_eq!(stream_ids, expected_stream_ids);
            assert_eq!(
                manager
                    .get_hub(&stream_id)
                    .expect("hub should exist")
                    .list_receivers()
                    .len(),
                expected.len()
            );

            for (receiver_id, expected_status) in expected {
                let receiver = manager
                    .get_receiver(&receiver_id)
                    .expect("aggregated receiver should be addressable by id");
                assert_eq!(receiver.stream_id, stream_id);
                assert_eq!(receiver.status, expected_status);
            }
        }

        for removed_id in removed_ids {
            assert!(manager.get_receiver(&removed_id).is_none());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn publisher_acquire_allows_only_one_concurrent_owner_per_stream() {
        const PUBLISHERS: usize = 32;

        let manager = Arc::new(StreamManager::new());
        manager.create_stream(
            "live",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );

        let mut tasks = Vec::new();
        for idx in 0..PUBLISHERS {
            let manager = Arc::clone(&manager);
            tasks.push(tokio::spawn(async move {
                let publisher_id = format!("publisher-{idx}");
                let acquired = manager.acquire_publisher("live", &publisher_id).is_ok();
                (publisher_id, acquired)
            }));
        }

        let mut winners = Vec::new();
        let mut losers = Vec::new();
        for task in tasks {
            let (publisher_id, acquired) = task.await.expect("publisher task should finish");
            if acquired {
                winners.push(publisher_id);
            } else {
                losers.push(publisher_id);
            }
        }

        assert_eq!(winners.len(), 1);
        assert_eq!(losers.len(), PUBLISHERS - 1);
        assert_eq!(manager.current_publisher("live"), Some(winners[0].clone()));

        assert!(!manager.release_publisher("live", &losers[0]));
        assert_eq!(manager.current_publisher("live"), Some(winners[0].clone()));

        assert!(manager.release_publisher("live", &winners[0]));
        assert!(manager.current_publisher("live").is_none());
        assert!(manager.acquire_publisher("live", "publisher-next").is_ok());
        assert_eq!(
            manager.current_publisher("live").as_deref(),
            Some("publisher-next")
        );
    }
}
