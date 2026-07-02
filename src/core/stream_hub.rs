//! Per-stream hub: stream metadata, playback receivers, FrameRing storage + seq notification.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::watch;

use super::frame_ring::{FrameRing, SnapMode, StoredFrame};
use super::{
    MediaFrame, PlaybackStatus, ReceiverId, ReceiverStatus, Stream, StreamId, StreamProtocol,
    StreamReceiver, StreamSourceMode, StreamStatus, Track,
};

pub struct StreamHub {
    pub stream_id: StreamId,
    stream: RwLock<Stream>,
    receivers: RwLock<HashMap<ReceiverId, StreamReceiver>>,
    publisher: RwLock<Option<String>>,
    ring: RwLock<FrameRing>,
    seq_tx: watch::Sender<u64>,
}

impl StreamHub {
    pub fn new(stream_id: &str) -> Arc<Self> {
        Self::with_stream(Stream {
            id: stream_id.to_string(),
            tracks: Vec::new(),
            status: StreamStatus::Created,
            playback_status: PlaybackStatus::Idle,
            source: StreamSourceMode::Push,
            protocol: StreamProtocol::Unknown,
            pull_url: None,
            sps: None,
            pps: None,
        })
    }

    pub(crate) fn with_stream(stream: Stream) -> Arc<Self> {
        let (seq_tx, _) = watch::channel(0u64);
        let stream_id = stream.id.clone();
        Arc::new(Self {
            stream_id,
            stream: RwLock::new(stream),
            receivers: RwLock::new(HashMap::new()),
            publisher: RwLock::new(None),
            ring: RwLock::new(FrameRing::new()),
            seq_tx,
        })
    }

    pub fn stream(&self) -> Stream {
        self.stream.read().clone()
    }

    pub(crate) fn update_stream<R>(&self, f: impl FnOnce(&mut Stream) -> R) -> R {
        let mut stream = self.stream.write();
        f(&mut stream)
    }

    pub(crate) fn set_tracks(&self, tracks: Vec<Track>) {
        self.update_stream(|stream| {
            stream.tracks = tracks;
        });
    }

    pub(crate) fn add_receiver(&self, receiver: StreamReceiver) -> StreamReceiver {
        self.receivers
            .write()
            .insert(receiver.id.clone(), receiver.clone());
        receiver
    }

    pub(crate) fn remove_receiver(&self, receiver_id: &ReceiverId) -> Option<StreamReceiver> {
        self.receivers.write().remove(receiver_id)
    }

    pub(crate) fn get_receiver(&self, receiver_id: &ReceiverId) -> Option<StreamReceiver> {
        self.receivers.read().get(receiver_id).cloned()
    }

    pub(crate) fn list_receiver_ids(&self) -> Vec<ReceiverId> {
        self.receivers.read().keys().cloned().collect()
    }

    pub(crate) fn list_receivers(&self) -> Vec<StreamReceiver> {
        self.receivers.read().values().cloned().collect()
    }

    pub(crate) fn set_receiver_status(
        &self,
        receiver_id: &ReceiverId,
        status: ReceiverStatus,
    ) -> Option<ReceiverStatus> {
        let mut receivers = self.receivers.write();
        receivers.get_mut(receiver_id).map(|receiver| {
            let old_status = receiver.status.clone();
            receiver.status = status;
            old_status
        })
    }

    pub(crate) fn acquire_publisher(&self, publisher_id: &str) -> Result<(), String> {
        let mut publisher = self.publisher.write();
        match publisher.as_deref() {
            Some(current) if current != publisher_id => Err(current.to_string()),
            _ => {
                *publisher = Some(publisher_id.to_string());
                Ok(())
            }
        }
    }

    pub(crate) fn release_publisher(&self, publisher_id: &str) -> bool {
        let mut publisher = self.publisher.write();
        if publisher.as_deref() == Some(publisher_id) {
            *publisher = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn current_publisher(&self) -> Option<String> {
        self.publisher.read().clone()
    }

    pub fn publish(&self, frame: MediaFrame) -> u64 {
        let seq = self.ring.write().push(frame);
        let _ = self.seq_tx.send(seq);
        seq
    }

    pub fn subscribe_notify(&self) -> watch::Receiver<u64> {
        self.seq_tx.subscribe()
    }

    pub fn get(&self, seq: u64) -> Option<MediaFrame> {
        self.ring.read().get(seq).map(|f| f.to_media_frame())
    }

    pub fn get_stored(&self, seq: u64) -> Option<StoredFrame> {
        self.ring.read().get(seq).cloned()
    }

    pub fn latest_seq(&self) -> u64 {
        self.ring.read().latest_seq()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.read().is_empty()
    }

    pub fn snap(&self, mode: SnapMode) -> u64 {
        self.ring.read().snap(mode)
    }

    pub fn latest_idr_frame(&self) -> Option<MediaFrame> {
        self.ring.read().latest_idr_frame()
    }

    pub fn latest_idr_seq(&self) -> Option<u64> {
        self.ring.read().latest_idr_seq()
    }

    pub fn latest_idr_bytes(&self) -> Option<(Vec<u8>, u64)> {
        self.ring
            .read()
            .latest_idr_frame()
            .map(|f| (f.data.to_vec(), f.timestamp))
    }

    pub fn frames_from(&self, from_seq: u64, to_seq: u64) -> Vec<MediaFrame> {
        self.ring.read().frames_from(from_seq, to_seq)
    }

    pub fn oldest_seq(&self) -> Option<u64> {
        self.ring.read().oldest_seq()
    }

    pub fn reset(&self) {
        *self.ring.write() = FrameRing::new();
        let _ = self.seq_tx.send(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::collections::HashMap;

    fn h264_frame(stream_id: &str, timestamp: u64, keyframe: bool) -> MediaFrame {
        let nal = if keyframe { 0x65 } else { 0x41 };
        MediaFrame::new(
            stream_id.to_string(),
            0,
            timestamp,
            Bytes::from(vec![0, 0, 0, 1, nal]),
            keyframe,
            super::super::CodecType::H264,
        )
    }

    fn stream_with_metadata() -> Stream {
        Stream {
            id: "s1".to_string(),
            tracks: vec![Track {
                id: 0,
                codec: super::super::CodecType::H264,
                payload_type: 96,
                clock_rate: 90_000,
                extra_params: HashMap::from([(
                    "profile-level-id".to_string(),
                    "42e01f".to_string(),
                )]),
            }],
            status: StreamStatus::Publishing,
            playback_status: PlaybackStatus::Playing,
            source: StreamSourceMode::Pull,
            protocol: StreamProtocol::RTSP,
            pull_url: Some("rtsp://example/live".to_string()),
            sps: Some(vec![0x67, 0x42]),
            pps: Some(vec![0x68, 0xce]),
        }
    }

    #[test]
    fn new_initializes_default_stream_and_empty_ring() {
        let hub = StreamHub::new("live");

        let stream = hub.stream();
        assert_eq!(hub.stream_id, "live");
        assert_eq!(stream.id, "live");
        assert_eq!(stream.status, StreamStatus::Created);
        assert_eq!(stream.playback_status, PlaybackStatus::Idle);
        assert_eq!(stream.source, StreamSourceMode::Push);
        assert_eq!(stream.protocol, StreamProtocol::Unknown);
        assert!(stream.tracks.is_empty());
        assert!(hub.is_empty());
        assert_eq!(hub.latest_seq(), 0);
        assert_eq!(hub.oldest_seq(), None);
        assert!(hub.latest_idr_frame().is_none());
    }

    #[test]
    fn with_stream_preserves_metadata_and_stream_returns_snapshot() {
        let hub = StreamHub::with_stream(stream_with_metadata());

        let mut snapshot = hub.stream();
        assert_eq!(snapshot.source, StreamSourceMode::Pull);
        assert_eq!(snapshot.protocol, StreamProtocol::RTSP);
        assert_eq!(snapshot.pull_url.as_deref(), Some("rtsp://example/live"));
        assert_eq!(snapshot.tracks.len(), 1);
        assert_eq!(snapshot.sps.as_deref(), Some(&[0x67, 0x42][..]));

        snapshot.status = StreamStatus::Stopped;
        snapshot.tracks.clear();

        let current = hub.stream();
        assert_eq!(current.status, StreamStatus::Publishing);
        assert_eq!(current.tracks.len(), 1);
    }

    #[test]
    fn update_stream_and_set_tracks_mutate_held_stream() {
        let hub = StreamHub::new("live");
        let tracks = vec![Track::new(1, super::super::CodecType::AAC, 97, 44_100)];

        hub.set_tracks(tracks);
        hub.update_stream(|stream| {
            stream.status = StreamStatus::Paused;
            stream.playback_status = PlaybackStatus::Playing;
            stream.sps = Some(vec![0x67]);
        });

        let stream = hub.stream();
        assert_eq!(stream.tracks.len(), 1);
        assert_eq!(stream.tracks[0].codec, super::super::CodecType::AAC);
        assert_eq!(stream.status, StreamStatus::Paused);
        assert_eq!(stream.playback_status, PlaybackStatus::Playing);
        assert_eq!(stream.sps.as_deref(), Some(&[0x67][..]));
    }

    #[test]
    fn receiver_lifecycle_is_scoped_to_hub() {
        let hub = StreamHub::new("live");
        let receiver = StreamReceiver::new(
            "live",
            super::super::StreamSinkMode::Pull,
            StreamProtocol::RTSP,
        )
        .with_client_addr("127.0.0.1:554");
        let receiver_id = receiver.id.clone();

        let added = hub.add_receiver(receiver.clone());
        assert_eq!(added.id, receiver_id);
        assert_eq!(hub.list_receiver_ids(), vec![receiver_id.clone()]);
        assert_eq!(hub.list_receivers().len(), 1);
        assert_eq!(
            hub.get_receiver(&receiver_id)
                .expect("receiver")
                .client_addr
                .as_deref(),
            Some("127.0.0.1:554")
        );

        let old_status = hub
            .set_receiver_status(&receiver_id, ReceiverStatus::Playing)
            .expect("status should update");
        assert_eq!(old_status, ReceiverStatus::Idle);
        assert_eq!(
            hub.get_receiver(&receiver_id).expect("receiver").status,
            ReceiverStatus::Playing
        );

        let removed = hub
            .remove_receiver(&receiver_id)
            .expect("receiver should be removed");
        assert_eq!(removed.id, receiver_id);
        assert!(hub.get_receiver(&receiver_id).is_none());
        assert!(hub.list_receiver_ids().is_empty());
        assert!(hub
            .set_receiver_status(&receiver_id, ReceiverStatus::Paused)
            .is_none());
    }

    #[test]
    fn publisher_slot_allows_single_owner_and_requires_matching_release() {
        let hub = StreamHub::new("live");

        hub.acquire_publisher("rtmp:one")
            .expect("first publisher should acquire slot");
        assert_eq!(hub.current_publisher().as_deref(), Some("rtmp:one"));
        assert!(hub.acquire_publisher("rtmp:one").is_ok());
        assert_eq!(
            hub.acquire_publisher("rtsp:two").unwrap_err(),
            "rtmp:one".to_string()
        );

        assert!(!hub.release_publisher("rtsp:two"));
        assert_eq!(hub.current_publisher().as_deref(), Some("rtmp:one"));
        assert!(hub.release_publisher("rtmp:one"));
        assert!(hub.current_publisher().is_none());
        assert!(hub.acquire_publisher("rtsp:two").is_ok());
    }

    #[tokio::test]
    async fn publish_updates_ring_and_notifies_subscribers() {
        let hub = StreamHub::new("live");
        let mut notify = hub.subscribe_notify();

        let seq = hub.publish(h264_frame("live", 1000, true));
        notify.changed().await.expect("publish should notify");

        assert_eq!(seq, 0);
        assert!(!hub.is_empty());
        assert_eq!(hub.latest_seq(), 0);
        assert_eq!(hub.oldest_seq(), Some(0));
        assert_eq!(hub.snap(SnapMode::LatestIdr), 0);
        assert_eq!(hub.get(0).expect("frame").timestamp, 1000);
        assert_eq!(hub.get_stored(0).expect("stored").seq, 0);
        assert_eq!(hub.latest_idr_bytes(), Some((vec![0, 0, 0, 1, 0x65], 1000)));
    }

    #[tokio::test]
    async fn reset_clears_ring_but_preserves_stream_metadata() {
        let hub = StreamHub::with_stream(stream_with_metadata());
        let mut notify = hub.subscribe_notify();
        hub.publish(h264_frame("s1", 1000, true));
        notify.changed().await.expect("publish should notify");

        hub.reset();
        notify.changed().await.expect("reset should notify");

        assert!(hub.is_empty());
        assert_eq!(hub.oldest_seq(), None);
        assert!(hub.latest_idr_frame().is_none());
        assert!(hub.get(0).is_none());
        let stream = hub.stream();
        assert_eq!(stream.status, StreamStatus::Publishing);
        assert_eq!(stream.sps.as_deref(), Some(&[0x67, 0x42][..]));
    }
}
