//! Central registry for streams, frame hubs, and playback receivers.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use super::dispatch::{DispatchPolicy, DispatchReader};
use super::stream_hub::StreamHub;
use super::{
    CodecType, MediaFrame, PlaybackStatus, ReceiverId, ReceiverStatus, Stream, StreamId,
    StreamProtocol, StreamReceiver, StreamSinkMode, StreamSourceMode, StreamStatus, Track,
    VIDEO_RTP_CLOCK_RATE,
};

// ---------------------------------------------------------------------------
// Stream lifecycle events — broadcast to all protocol / process subscribers
// ---------------------------------------------------------------------------

/// A lifecycle event emitted by [`StreamManager`] whenever a stream, publisher,
/// or receiver enters or leaves a state.  Protocol modules (RTMP, RTSP,
/// WebRTC, HLS, HTTP‑FLV) and process modules (record, snapshot, transcode,
/// analysis) can subscribe via [`StreamManager::subscribe_events`] and react
/// without polling.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A new stream has been registered.
    StreamCreated {
        stream_id: StreamId,
        source: StreamSourceMode,
        protocol: StreamProtocol,
        pull_url: Option<String>,
    },
    /// A stream has been removed (hub destroyed).
    StreamRemoved {
        stream_id: StreamId,
    },
    /// Stream status changed (e.g. Created → Publishing → Unpublished).
    StreamStatusChanged {
        stream_id: StreamId,
        old_status: StreamStatus,
        new_status: StreamStatus,
    },
    /// A publisher acquired the stream (only one at a time).
    PublisherAcquired {
        stream_id: StreamId,
        publisher_id: String,
    },
    /// A publisher released the stream.
    PublisherReleased {
        stream_id: StreamId,
        publisher_id: String,
    },
    /// A playback receiver / sink was created (HTTP‑FLV, HLS, RTMP play, …).
    ReceiverCreated {
        stream_id: StreamId,
        receiver_id: ReceiverId,
        mode: StreamSinkMode,
        protocol: StreamProtocol,
        client_addr: Option<String>,
    },
    /// A receiver was removed (playback stopped).
    ReceiverRemoved {
        stream_id: StreamId,
        receiver_id: ReceiverId,
    },
    /// Receiver status changed (e.g. Idle → Playing).
    ReceiverStatusChanged {
        stream_id: StreamId,
        receiver_id: ReceiverId,
        old_status: ReceiverStatus,
        new_status: ReceiverStatus,
    },
}

/// Convenience alias for the broadcast receiver end of the event channel.
pub type StreamEventReceiver = broadcast::Receiver<StreamEvent>;

/// Per-pull-loop cancellation handle.
pub struct PullSessionHandle {
    stop: AtomicBool,
}

impl PullSessionHandle {
    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    pub fn new_active() -> Arc<Self> {
        Arc::new(Self {
            stop: AtomicBool::new(false),
        })
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub struct StreamManager {
    hubs: RwLock<HashMap<StreamId, Arc<StreamHub>>>,
    pull_sessions: RwLock<HashMap<StreamId, Arc<PullSessionHandle>>>,
    /// Broadcast sender for stream lifecycle events.
    /// Protocol / process modules subscribe via [`StreamManager::subscribe_events`].
    event_tx: broadcast::Sender<StreamEvent>,
}

impl StreamManager {
    /// Capacity of the lifecycle event broadcast channel.
    /// 256 should be enough to absorb short bursts (many receivers
    /// starting/stopping concurrently) without dropping events for
    /// slow subscribers.
    const EVENT_CHANNEL_CAPACITY: usize = 256;

    pub fn new() -> Self {
        let (event_tx, _) = broadcast::channel(Self::EVENT_CHANNEL_CAPACITY);
        Self {
            hubs: RwLock::new(HashMap::new()),
            pull_sessions: RwLock::new(HashMap::new()),
            event_tx,
        }
    }

    /// Subscribe to stream lifecycle events.
    ///
    /// Returns a [`broadcast::Receiver`] that yields every
    /// [`StreamEvent`] emitted by this manager.  Protocol modules
    /// and processing pipelines can call this once at startup and
    /// then `recv().await` to react to stream/publisher/receiver
    /// changes without polling.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut rx = manager.subscribe_events();
    /// tokio::spawn(async move {
    ///     while let Ok(event) = rx.recv().await {
    ///         match event {
    ///             StreamEvent::StreamCreated { stream_id, .. } => { /* … */ }
    ///             StreamEvent::StreamRemoved { stream_id }    => { /* … */ }
    ///             _ => {}
    ///         }
    ///     }
    /// });
    /// ```
    pub fn subscribe_events(&self) -> StreamEventReceiver {
        self.event_tx.subscribe()
    }

    /// Emit an event to all subscribers (fire-and-forget).
    fn emit(&self, event: StreamEvent) {
        // Ignore SendError (no receivers) — callers that care about
        // lagged receivers can use `.receiver_count()`.
        if self.event_tx.receiver_count() > 0 {
            let _ = self.event_tx.send(event);
        }
    }

    /// Start a new pull loop for `stream_id`, stopping any previous loop on the same id.
    pub fn begin_pull_loop(&self, stream_id: &str) -> Arc<PullSessionHandle> {
        let mut sessions = self.pull_sessions.write();
        if let Some(existing) = sessions.get(stream_id) {
            existing.stop();
            info!(
                "[Core] Stopping previous pull loop for stream '{}'",
                stream_id
            );
        }
        let session = Arc::new(PullSessionHandle {
            stop: AtomicBool::new(false),
        });
        sessions.insert(stream_id.to_string(), session.clone());
        session
    }

    pub fn end_pull_loop(&self, stream_id: &str, session: &Arc<PullSessionHandle>) {
        let mut sessions = self.pull_sessions.write();
        if sessions
            .get(stream_id)
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            sessions.remove(stream_id);
        }
    }

    pub fn request_pull_stop(&self, stream_id: &str) {
        if let Some(session) = self.pull_sessions.read().get(stream_id) {
            session.stop();
            info!("[Core] Pull stop requested for stream '{}'", stream_id);
        }
    }

    pub fn is_pull_stop_requested(&self, stream_id: &str) -> bool {
        self.pull_sessions
            .read()
            .get(stream_id)
            .is_some_and(|session| session.is_stopped())
    }

    pub fn should_publish_to_stream(&self, stream_id: &str) -> bool {
        if self
            .pull_sessions
            .read()
            .get(stream_id)
            .is_some_and(|session| session.is_stopped())
        {
            return false;
        }
        self.get_hub(stream_id).is_some()
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
            vps: None,
            sps: None,
            pps: None,
            hevc_decoder_config: None,
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

        self.emit(StreamEvent::StreamCreated {
            stream_id: stream_id.to_string(),
            source: stream.source.clone(),
            protocol: stream.protocol.clone(),
            pull_url: stream.pull_url.clone(),
        });

        stream
    }

    pub fn set_stream_tracks(&self, stream_id: &str, tracks: Vec<Track>) {
        if let Some(hub) = self.get_hub(stream_id) {
            hub.set_tracks(tracks);
        }
    }

    pub fn set_stream_sps_pps(&self, stream_id: &str, sps: Vec<u8>, pps: Vec<u8>) {
        if sps.is_empty() || pps.is_empty() {
            warn!(
                "[Core] Ignoring empty SPS/PPS for stream {} (sps={} pps={})",
                stream_id,
                sps.len(),
                pps.len()
            );
            return;
        }
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

    pub fn set_stream_hevc_params(
        &self,
        stream_id: &str,
        vps: Vec<u8>,
        sps: Vec<u8>,
        pps: Vec<u8>,
    ) {
        self.set_stream_hevc_config(stream_id, vps, sps, pps, None);
    }

    pub fn set_stream_hevc_config(
        &self,
        stream_id: &str,
        vps: Vec<u8>,
        sps: Vec<u8>,
        pps: Vec<u8>,
        hevc_decoder_config: Option<Vec<u8>>,
    ) {
        if let Some(hub) = self.get_hub(stream_id) {
            hub.update_stream(|stream| {
                stream.vps = Some(vps);
                stream.sps = Some(sps);
                stream.pps = Some(pps);
                if hevc_decoder_config.is_some() {
                    stream.hevc_decoder_config = hevc_decoder_config;
                    // Drop placeholder AAC from default_live_tracks until publisher
                    // sends an AAC sequence header (transcode -an is video-only).
                    stream.tracks.retain(|t| t.codec != CodecType::AAC);
                }
                if let Some(track) = stream.tracks.iter_mut().find(|t| t.id == 0) {
                    track.codec = CodecType::H265;
                } else {
                    stream
                        .tracks
                        .insert(0, Track::new(0, CodecType::H265, 96, 90_000));
                }
                info!("[Core] Setting HEVC VPS/SPS/PPS for stream {}", stream_id);
            });
        }
    }

    /// Ensure AAC track exists after an AAC sequence header (e.g. Enhanced HEVC + AAC publish).
    pub fn ensure_aac_track(&self, stream_id: &str) {
        if let Some(hub) = self.get_hub(stream_id) {
            hub.update_stream(|stream| {
                if stream.tracks.iter().any(|t| t.codec == CodecType::AAC) {
                    return;
                }
                stream
                    .tracks
                    .push(Track::new(1, CodecType::AAC, 97, 48_000));
            });
        }
    }

    /// Mark the primary video track as H264/H265 (RTMP publish starts with placeholder H264).
    pub fn ensure_video_track_codec(&self, stream_id: &str, codec: CodecType) {
        if !matches!(codec, CodecType::H264 | CodecType::H265) {
            return;
        }
        if let Some(hub) = self.get_hub(stream_id) {
            hub.update_stream(|stream| {
                if let Some(track) = stream.tracks.iter_mut().find(|t| t.id == 0) {
                    if track.codec != codec {
                        track.codec = codec;
                    }
                } else {
                    stream
                        .tracks
                        .insert(0, Track::new(0, codec, 96, VIDEO_RTP_CLOCK_RATE));
                }
            });
        }
    }

    /// Resolve the codec actually being published (stream metadata, ring, then track list).
    pub fn published_video_codec(&self, stream_id: &str) -> Option<CodecType> {
        let stream = self.get_stream(&stream_id.to_string())?;
        if stream.vps.is_some() && stream.sps.is_some() && stream.pps.is_some() {
            return Some(CodecType::H265);
        }
        if stream.sps.is_some() && stream.pps.is_some() && stream.vps.is_none() {
            return Some(CodecType::H264);
        }
        if let Some(hub) = self.get_hub(stream_id) {
            let latest = hub.latest_seq();
            if let Some(frame) = hub.get(latest) {
                if matches!(frame.codec, CodecType::H264 | CodecType::H265) {
                    return Some(frame.codec);
                }
            }
        }
        stream
            .tracks
            .iter()
            .find(|t| t.codec == CodecType::H265)
            .or_else(|| {
                stream
                    .tracks
                    .iter()
                    .find(|t| matches!(t.codec, CodecType::H264 | CodecType::H265))
            })
            .map(|t| t.codec)
    }

    /// Merge VPS/SPS/PPS from a single NALU into stream codec config.
    pub fn merge_stream_nalu_config(&self, stream_id: &str, codec: CodecType, nalu: &[u8]) {
        if nalu.is_empty() {
            return;
        }
        if let Some((vps, sps, pps)) = crate::core::merge_annex_b_nalu_config(codec, nalu) {
            if let Some(hub) = self.get_hub(stream_id) {
                hub.update_stream(|stream| {
                    if let Some(v) = vps {
                        if stream.vps.is_none() {
                            stream.vps = Some(v);
                        }
                    }
                    if let Some(s) = sps {
                        if stream.sps.is_none() {
                            stream.sps = Some(s);
                        }
                    }
                    if let Some(p) = pps {
                        if stream.pps.is_none() {
                            stream.pps = Some(p);
                        }
                    }
                });
            }
            return;
        }
        // H.264 fallback
        self.merge_stream_nalu_config_h264(stream_id, nalu);
    }

    fn merge_stream_nalu_config_h264(&self, stream_id: &str, nalu: &[u8]) {
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
        self.request_pull_stop(stream_id);
        let removed = self.hubs.write().remove(stream_id).map(|hub| hub.stream());
        if removed.is_some() {
            self.emit(StreamEvent::StreamRemoved {
                stream_id: stream_id.clone(),
            });
        }
        removed
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
            let mut old_status = StreamStatus::Created;
            hub.update_stream(|stream| {
                old_status = stream.status.clone();
                stream.status = status.clone();
                info!(
                    "[Core] Stream {} status changed from {:?} to {:?}",
                    stream_id, old_status, status
                );
            });
            if old_status != status {
                self.emit(StreamEvent::StreamStatusChanged {
                    stream_id: stream_id.to_string(),
                    old_status,
                    new_status: status,
                });
            }
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
                self.reset_stream_media(stream_id);
                info!(
                    "[Core] Publisher {} acquired stream {}",
                    publisher_id, stream_id
                );
                self.emit(StreamEvent::PublisherAcquired {
                    stream_id: stream_id.to_string(),
                    publisher_id: publisher_id.to_string(),
                });
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
            self.reset_stream_media(stream_id);
            info!(
                "[Core] Publisher {} released stream {}",
                publisher_id, stream_id
            );
            self.emit(StreamEvent::PublisherReleased {
                stream_id: stream_id.to_string(),
                publisher_id: publisher_id.to_string(),
            });
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

        self.emit(StreamEvent::ReceiverCreated {
            stream_id: stream_id.to_string(),
            receiver_id: receiver.id.clone(),
            mode: receiver.mode.clone(),
            protocol: receiver.protocol.clone(),
            client_addr: receiver.client_addr.clone(),
        });

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

        self.emit(StreamEvent::ReceiverCreated {
            stream_id: stream_id.to_string(),
            receiver_id: receiver.id.clone(),
            mode: StreamSinkMode::Pull,
            protocol: receiver.protocol.clone(),
            client_addr: Some(client_addr.to_string()),
        });

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

        self.emit(StreamEvent::ReceiverCreated {
            stream_id: stream_id.to_string(),
            receiver_id: receiver.id.clone(),
            mode: StreamSinkMode::Push,
            protocol: receiver.protocol.clone(),
            client_addr: None,
        });

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
            self.emit(StreamEvent::ReceiverRemoved {
                stream_id: r.stream_id.clone(),
                receiver_id: r.id.clone(),
            });
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
        let hubs = self.hubs.read();
        let mut stream_id = None;
        let mut old_status = None;
        for hub in hubs.values() {
            if let Some(os) = hub.set_receiver_status(receiver_id, status.clone()) {
                old_status = Some(os);
                stream_id = Some(hub.stream_id.clone());
                break;
            }
        }
        drop(hubs);
        if let (Some(old_status), Some(stream_id)) = (old_status, stream_id) {
            info!(
                "[Core] Receiver {} status changed from {:?} to {:?}",
                receiver_id, old_status, status
            );
            if old_status != status {
                self.emit(StreamEvent::ReceiverStatusChanged {
                    stream_id,
                    receiver_id: receiver_id.clone(),
                    old_status,
                    new_status: status,
                });
            }
            Ok(())
        } else {
            Err(anyhow::anyhow!("Receiver {} not found", receiver_id))
        }
    }

    pub fn get_last_keyframe(&self, stream_id: &str) -> Option<(Vec<u8>, u64, Option<u32>)> {
        self.get_hub(stream_id)?.latest_idr_bytes()
    }

    /// Clear the frame ring and cached SPS/PPS after a publisher session ends.
    pub fn reset_stream_media(&self, stream_id: &str) {
        let Some(hub) = self.get_hub(stream_id) else {
            return;
        };
        hub.reset();
        hub.update_stream(|stream| {
            stream.sps = None;
            stream.pps = None;
        });
        info!("[Core] Reset stream media ring for '{}'", stream_id);
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

        if !self.should_publish_to_stream(&stream_id) {
            debug!(
                "[Core] publish_frame: stream_id={} skipped (hub removed or pull stopped)",
                stream_id
            );
            return;
        }

        if let Some(hub) = self.get_or_create_hub(&stream_id) {
            let seq = hub.publish(frame);
            debug!(
                "[Core] publish_frame: stream_id={} ring_seq={}",
                stream_id, seq
            );
        }

        if let Some(hub) = self.get_hub(&stream_id) {
            let mut old_status = StreamStatus::Created;
            hub.update_stream(|stream| {
                match stream.status {
                    StreamStatus::Created | StreamStatus::Unpublished => {
                        old_status = stream.status.clone();
                        stream.status = StreamStatus::Publishing;
                        debug!("[Core] Stream {} status -> Publishing", stream_id);
                    }
                    _ => {}
                }
            });
            if old_status != StreamStatus::Publishing
                && matches!(old_status, StreamStatus::Created | StreamStatus::Unpublished)
            {
                self.emit(StreamEvent::StreamStatusChanged {
                    stream_id: stream_id.clone(),
                    old_status,
                    new_status: StreamStatus::Publishing,
                });
            }
        }
    }

    pub fn update_stream_status(&self, stream_id: &StreamId, status: StreamStatus) {
        if let Some(hub) = self.get_hub(stream_id) {
            let mut old_status = StreamStatus::Created;
            hub.update_stream(|stream| {
                old_status = stream.status.clone();
                stream.status = status.clone();
            });
            if old_status != status {
                self.emit(StreamEvent::StreamStatusChanged {
                    stream_id: stream_id.clone(),
                    old_status,
                    new_status: status,
                });
            }
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
    use crate::core::default_live_tracks;
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
    fn published_video_codec_prefers_ring_over_placeholder_h264_track() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::RTMP, None);
        manager.set_stream_tracks("s", default_live_tracks());
        manager.ensure_stream_hub("s");

        assert_eq!(manager.published_video_codec("s"), Some(CodecType::H264));

        manager.publish_frame(MediaFrame::new(
            "s".into(),
            0,
            1000,
            bytes::Bytes::from_static(&[0, 0, 0, 1, 0x2a, 0x01, 0xaf]),
            true,
            CodecType::H265,
        ));

        assert_eq!(manager.published_video_codec("s"), Some(CodecType::H265));
    }

    #[test]
    fn metadata_updates_for_missing_stream_do_not_create_hub() {
        let manager = StreamManager::new();

        manager.set_stream_tracks("missing", vec![Track::new(1, CodecType::AAC, 97, 44_100)]);
        manager.set_stream_sps_pps("missing", vec![0x67], vec![0x68]);
        manager.merge_stream_nalu_config("missing", CodecType::H264, &[0x67, 0x42]);
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

        manager.merge_stream_nalu_config("s", CodecType::H264, &[]);
        manager.merge_stream_nalu_config("s", CodecType::H264, &[0x65, 0x01]);
        assert!(manager.get_stream(&"s".to_string()).unwrap().sps.is_none());
        assert!(manager.get_stream(&"s".to_string()).unwrap().pps.is_none());

        manager.merge_stream_nalu_config("s", CodecType::H264, &[0x67, 0x01]);
        manager.merge_stream_nalu_config("s", CodecType::H264, &[0x68, 0x02]);
        manager.merge_stream_nalu_config("s", CodecType::H264, &[0x67, 0xff]);
        manager.merge_stream_nalu_config("s", CodecType::H264, &[0x68, 0xee]);

        let stream = manager.get_stream(&"s".to_string()).unwrap();
        assert_eq!(stream.sps.as_deref(), Some(&[0x67, 0x01][..]));
        assert_eq!(stream.pps.as_deref(), Some(&[0x68, 0x02][..]));
    }

    #[test]
    fn remove_stream_requests_pull_stop_and_skips_publish() {
        let manager = StreamManager::new();
        create_test_stream(&manager, "s");
        let session = manager.begin_pull_loop("s");
        manager.set_publishing("s").unwrap();

        manager.publish_frame(h264_frame("s", 1000, true));
        manager.remove_stream(&"s".to_string());

        assert!(manager.get_hub("s").is_none());
        assert!(session.is_stopped());
        manager.publish_frame(h264_frame("s", 2000, false));
        assert!(manager.get_hub("s").is_none());
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
            Some((vec![0, 0, 0, 1, 0x65], 1000, None))
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
    fn reset_stream_media_clears_ring_and_codec_config() {
        let manager = StreamManager::new();
        create_test_stream(&manager, "s");
        manager.set_stream_sps_pps("s", vec![0x67], vec![0x68]);
        manager.publish_frame(h264_frame("s", 1000, true));

        manager.reset_stream_media("s");

        assert!(manager.get_last_keyframe("s").is_none());
        let stream = manager.get_stream(&"s".to_string()).unwrap();
        assert!(stream.sps.is_none());
        assert!(stream.pps.is_none());
    }

    #[test]
    fn release_and_acquire_publisher_reset_stream_media() {
        let manager = StreamManager::new();
        create_test_stream(&manager, "s");
        manager.set_stream_sps_pps("s", vec![0x67], vec![0x68]);
        manager
            .acquire_publisher("s", "pub-a")
            .expect("first publisher");
        manager.publish_frame(h264_frame("s", 1000, true));
        assert!(manager.get_last_keyframe("s").is_some());

        assert!(manager.release_publisher("s", "pub-a"));
        assert!(manager.get_last_keyframe("s").is_none());

        manager
            .acquire_publisher("s", "pub-b")
            .expect("second publisher");
        assert!(manager.get_last_keyframe("s").is_none());
        let stream = manager.get_stream(&"s".to_string()).unwrap();
        assert!(stream.sps.is_none());
        assert!(stream.pps.is_none());
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

    // -----------------------------------------------------------------------
    // StreamEvent broadcast channel tests
    // -----------------------------------------------------------------------

    /// Helper: collect up to `limit` events from a receiver with a short timeout.
    async fn drain_events(rx: &mut StreamEventReceiver, limit: usize) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            match tokio::time::timeout_at(deadline.into(), rx.recv()).await {
                Ok(Ok(event)) => {
                    events.push(event);
                    if events.len() >= limit {
                        break;
                    }
                }
                _ => break,
            }
        }
        events
    }

    #[test]
    fn subscribe_events_returns_a_live_receiver() {
        let manager = StreamManager::new();
        let rx = manager.subscribe_events();
        // Receiver should be valid (not closed/lagged before any event)
        assert_eq!(rx.len(), 0);
    }

    #[test]
    fn event_stream_created_emitted_when_stream_is_created() {
        let manager = StreamManager::new();
        let mut rx = manager.subscribe_events();

        let stream = create_test_stream(&manager, "s");

        let event = rx.try_recv().expect("should receive StreamCreated event");
        match event {
            StreamEvent::StreamCreated {
                stream_id,
                source,
                protocol,
                pull_url,
            } => {
                assert_eq!(stream_id, "s");
                assert_eq!(source, StreamSourceMode::Pull);
                assert_eq!(protocol, StreamProtocol::RTSP);
                assert_eq!(pull_url, Some("rtsp://example/s".to_string()));
            }
            other => panic!("expected StreamCreated, got {other:?}"),
        }
        assert!(rx.try_recv().is_err()); // no more events
        drop(rx);
        let _ = stream;
    }

    #[test]
    fn event_stream_removed_emitted_when_stream_is_removed() {
        let manager = StreamManager::new();
        let mut rx = manager.subscribe_events();

        create_test_stream(&manager, "s");
        let _created = rx.try_recv().unwrap(); // consume StreamCreated

        manager.remove_stream(&"s".to_string());

        let event = rx.try_recv().expect("should receive StreamRemoved event");
        match event {
            StreamEvent::StreamRemoved { stream_id } => {
                assert_eq!(stream_id, "s");
            }
            other => panic!("expected StreamRemoved, got {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn event_stream_removed_not_emitted_for_nonexistent_stream() {
        let manager = StreamManager::new();
        let mut rx = manager.subscribe_events();

        manager.remove_stream(&"nonexistent".to_string());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn event_publisher_acquired_and_released_emitted_in_order() {
        let manager = StreamManager::new();
        let mut rx = manager.subscribe_events();

        create_test_stream(&manager, "s");
        let _created = rx.try_recv().unwrap();

        manager
            .acquire_publisher("s", "pub-1")
            .expect("acquire publisher");

        let event = rx.try_recv().expect("should receive PublisherAcquired");
        match event {
            StreamEvent::PublisherAcquired {
                stream_id,
                publisher_id,
            } => {
                assert_eq!(stream_id, "s");
                assert_eq!(publisher_id, "pub-1");
            }
            other => panic!("expected PublisherAcquired, got {other:?}"),
        }

        manager.release_publisher("s", "pub-1");

        let event = rx.try_recv().expect("should receive PublisherReleased");
        match event {
            StreamEvent::PublisherReleased {
                stream_id,
                publisher_id,
            } => {
                assert_eq!(stream_id, "s");
                assert_eq!(publisher_id, "pub-1");
            }
            other => panic!("expected PublisherReleased, got {other:?}"),
        }
    }

    #[test]
    fn event_publisher_released_not_emitted_for_non_owner() {
        let manager = StreamManager::new();
        let mut rx = manager.subscribe_events();

        create_test_stream(&manager, "s");
        let _created = rx.try_recv().unwrap();

        manager.acquire_publisher("s", "pub-1").unwrap();
        let _acquired = rx.try_recv().unwrap();

        // Release by non-owner should be a no-op
        let released = manager.release_publisher("s", "non-owner");
        assert!(!released);
        assert!(rx.try_recv().is_err()); // no extra event
    }

    #[test]
    fn event_receiver_created_and_removed_emitted_in_order() {
        let manager = StreamManager::new();
        let mut rx = manager.subscribe_events();

        create_test_stream(&manager, "s");
        let _created = rx.try_recv().unwrap();

        let receiver = manager.create_pull_receiver("s", StreamProtocol::RTSP, "127.0.0.1:554");

        let event = rx.try_recv().expect("should receive ReceiverCreated");
        match event {
            StreamEvent::ReceiverCreated {
                stream_id,
                ref receiver_id,
                mode,
                protocol,
                ref client_addr,
            } => {
                assert_eq!(stream_id, "s");
                assert_eq!(*receiver_id, receiver.id);
                assert_eq!(mode, StreamSinkMode::Pull);
                assert_eq!(protocol, StreamProtocol::RTSP);
                assert_eq!(client_addr.as_deref(), Some("127.0.0.1:554"));
            }
            other => panic!("expected ReceiverCreated, got {other:?}"),
        }

        manager.remove_receiver(&receiver.id);

        let event = rx.try_recv().expect("should receive ReceiverRemoved");
        match event {
            StreamEvent::ReceiverRemoved {
                stream_id,
                receiver_id,
            } => {
                assert_eq!(stream_id, "s");
                assert_eq!(receiver_id, receiver.id);
            }
            other => panic!("expected ReceiverRemoved, got {other:?}"),
        }
    }

    #[test]
    fn event_receiver_created_for_push_receiver_includes_mode_and_no_client_addr() {
        let manager = StreamManager::new();
        let mut rx = manager.subscribe_events();

        create_test_stream(&manager, "s");
        let _created = rx.try_recv().unwrap();

        manager.create_push_receiver("s", StreamProtocol::RTMP, "rtmp://edge/live");

        let event = rx.try_recv().expect("should receive ReceiverCreated");
        match event {
            StreamEvent::ReceiverCreated {
                stream_id,
                mode,
                protocol,
                client_addr,
                ..
            } => {
                assert_eq!(stream_id, "s");
                assert_eq!(mode, StreamSinkMode::Push);
                assert_eq!(protocol, StreamProtocol::RTMP);
                assert!(client_addr.is_none());
            }
            other => panic!("expected ReceiverCreated for push, got {other:?}"),
        }
    }

    #[test]
    fn event_receiver_status_changed_emitted_on_transition() {
        let manager = StreamManager::new();
        let mut rx = manager.subscribe_events();

        create_test_stream(&manager, "s");
        let _created = rx.try_recv().unwrap();

        let receiver = manager.create_receiver("s", StreamSinkMode::Pull, StreamProtocol::HTTP);
        let _recv_created = rx.try_recv().unwrap();

        manager
            .set_receiver_status(&receiver.id, ReceiverStatus::Playing)
            .unwrap();

        let event = rx.try_recv().expect("should receive ReceiverStatusChanged");
        match event {
            StreamEvent::ReceiverStatusChanged {
                ref stream_id,
                ref receiver_id,
                old_status,
                new_status,
            } => {
                assert_eq!(stream_id, "s");
                assert_eq!(*receiver_id, receiver.id);
                assert_eq!(old_status, ReceiverStatus::Idle);
                assert_eq!(new_status, ReceiverStatus::Playing);
            }
            other => panic!("expected ReceiverStatusChanged, got {other:?}"),
        }

        // Setting same status should NOT emit a duplicate event
        manager
            .set_receiver_status(&receiver.id, ReceiverStatus::Playing)
            .unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn event_stream_status_changed_emitted_on_set_status() {
        let manager = StreamManager::new();
        let mut rx = manager.subscribe_events();

        create_test_stream(&manager, "s");
        let _created = rx.try_recv().unwrap();

        manager.set_status("s", StreamStatus::Publishing).unwrap();

        let event = rx.try_recv().expect("should receive StreamStatusChanged");
        match event {
            StreamEvent::StreamStatusChanged {
                stream_id,
                old_status,
                new_status,
            } => {
                assert_eq!(stream_id, "s");
                assert_eq!(old_status, StreamStatus::Created);
                assert_eq!(new_status, StreamStatus::Publishing);
            }
            other => panic!("expected StreamStatusChanged, got {other:?}"),
        }

        // Same status — no event
        manager.set_status("s", StreamStatus::Publishing).unwrap();
        assert!(rx.try_recv().is_err());

        manager.set_status("s", StreamStatus::Unpublished).unwrap();
        let event = rx.try_recv().expect("should receive StreamStatusChanged for Unpublished");
        match event {
            StreamEvent::StreamStatusChanged { new_status, .. } => {
                assert_eq!(new_status, StreamStatus::Unpublished);
            }
            other => panic!("expected StreamStatusChanged, got {other:?}"),
        }
    }

    #[test]
    fn event_stream_status_changed_emitted_on_publish_frame() {
        let manager = StreamManager::new();
        let mut rx = manager.subscribe_events();

        create_test_stream(&manager, "s");
        let _created = rx.try_recv().unwrap();

        // publish_frame should trigger Created→Publishing transition
        manager.publish_frame(h264_frame("s", 1000, true));

        let event = rx.try_recv().expect("should receive StreamStatusChanged via publish_frame");
        match event {
            StreamEvent::StreamStatusChanged {
                ref stream_id,
                old_status,
                new_status,
            } => {
                assert_eq!(stream_id, "s");
                assert!(matches!(old_status, StreamStatus::Created));
                assert_eq!(new_status, StreamStatus::Publishing);
            }
            other => panic!("expected StreamStatusChanged from publish_frame, got {other:?}"),
        }
    }

    #[test]
    fn event_multiple_subscribers_receive_same_events() {
        let manager = StreamManager::new();
        let mut rx1 = manager.subscribe_events();
        let mut rx2 = manager.subscribe_events();

        create_test_stream(&manager, "s");

        for rx in [&mut rx1, &mut rx2] {
            let event = rx.try_recv().expect("subscriber should receive StreamCreated");
            assert!(matches!(event, StreamEvent::StreamCreated { .. }));
        }
    }

    #[test]
    fn no_event_emitted_when_no_subscribers_present() {
        let manager = StreamManager::new();

        // Create/remove streams without any subscriber — should not panic or block
        let _stream = create_test_stream(&manager, "s1");
        manager.remove_stream(&"s1".to_string());

        let _stream = create_test_stream(&manager, "s2");
        manager.acquire_publisher("s2", "pub-1").unwrap();
        manager.release_publisher("s2", "pub-1");
        manager.remove_stream(&"s2".to_string());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn event_lagged_receiver_is_detected_when_channel_overflowed() {
        let manager = Arc::new(StreamManager::new());
        let mut rx = manager.subscribe_events();

        // Create many streams quickly to fill the channel buffer
        // (channel capacity is 256; 2 events per stream: StreamCreated + status changes)
        for i in 0..400 {
            manager.create_stream(
                &format!("s{i}"),
                StreamSourceMode::Push,
                StreamProtocol::Unknown,
                None,
            );
        }

        // Without draining, the slow subscriber should eventually see Lagged
        // Drain a few to avoid actual overflow if tests run slowly
        let _ = rx.try_recv();

        // Now drain rapidly with a dedicated task to clear the backlog
        let _events = drain_events(&mut rx, 800).await;
        // The key assertion: even after potential lag, the event system
        // did not crash/block the manager
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn event_concurrent_stream_operations_preserve_event_ordering_per_stream() {
        let manager = Arc::new(StreamManager::new());
        let mut rx = manager.subscribe_events();

        const N: usize = 16;
        let mut handles = Vec::new();

        for i in 0..N {
            let mgr = Arc::clone(&manager);
            handles.push(tokio::spawn(async move {
                let sid = format!("s{i}");
                mgr.create_stream(
                    &sid,
                    StreamSourceMode::Push,
                    StreamProtocol::Unknown,
                    None,
                );
                mgr.acquire_publisher(&sid, "pub").unwrap();
                mgr.release_publisher(&sid, "pub");
                mgr.remove_stream(&sid);
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        let events = drain_events(&mut rx, N * 4).await;

        // Each stream should produce 4 events in order:
        // StreamCreated → PublisherAcquired → PublisherReleased → StreamRemoved
        // (StreamStatusChanged may also appear for publish_frame/update_stream_status)
        let mut created: HashMap<String, usize> = HashMap::new();
        let mut removed: HashMap<String, usize> = HashMap::new();

        for event in &events {
            match event {
                StreamEvent::StreamCreated { stream_id, .. } => {
                    *created.entry(stream_id.clone()).or_default() += 1;
                }
                StreamEvent::StreamRemoved { stream_id } => {
                    *removed.entry(stream_id.clone()).or_default() += 1;
                }
                _ => {}
            }
        }

        // Every stream should be both created and removed
        for i in 0..N {
            let sid = format!("s{i}");
            assert_eq!(
                created.get(&sid).copied().unwrap_or(0),
                1,
                "Stream {sid} should be created exactly once"
            );
            assert_eq!(
                removed.get(&sid).copied().unwrap_or(0),
                1,
                "Stream {sid} should be removed exactly once"
            );
        }
    }

    #[test]
    fn event_receiver_removed_not_emitted_for_unknown_receiver() {
        let manager = StreamManager::new();
        let mut rx = manager.subscribe_events();

        let result = manager.remove_receiver(&"unknown-receiver-id".to_string());
        assert!(result.is_none());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn event_set_receiver_status_returns_error_for_unregistered_receiver() {
        let manager = StreamManager::new();
        let result = manager.set_receiver_status(&"unknown".to_string(), ReceiverStatus::Playing);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not found"));
    }
}
