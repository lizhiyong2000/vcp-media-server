mod config;
mod tester;
mod pusher;
mod timestamp;
mod frame_ring;
mod stream_hub;
pub mod dispatch;
pub mod protocol;

pub use frame_ring::{FrameRing, SnapMode, is_playable_video, is_video_keyframe};
pub use stream_hub::StreamHub;
pub use dispatch::{DispatchPolicy, DispatchReader, DispatchError, coalesce_flv_batch};

use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use bytes::Bytes;
use tracing::{info, warn, debug};
use anyhow::Result;

pub use config::{Config, RtmpConfig, RtspConfig, WebrtcConfig, HttpConfig, StreamConfig, TrackConfig};
pub use tester::StreamTester;
pub use pusher::*;
pub use protocol::{ProtocolType, ProtocolInfo, ProtocolRegistry, StreamSink};
pub use timestamp::{flv_timestamp_ms, media_timestamp_delta_ms, FlvPlayTimeline};

pub type StreamId = String;
pub type TrackId = u8;

#[derive(Debug, Clone)]
pub struct Stream {
    pub id: StreamId,
    pub tracks: Vec<Track>,
    pub status: StreamStatus,
    pub playback_status: PlaybackStatus,
    pub source: StreamSourceMode,
    pub protocol: StreamProtocol,
    pub pull_url: Option<String>,
    // Codec parameters extracted from RTP stream
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

impl Stream {
    pub fn add_track(&mut self, track: Track) {
        self.tracks.push(track);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamStatus {
    Created,
    Unpublished,
    Publishing,
    Paused,
    Stopped,
    Error(String),
}

impl StreamStatus {
    pub fn as_str(&self) -> &str {
        match self {
            StreamStatus::Created => "created",
            StreamStatus::Unpublished => "unpublished",
            StreamStatus::Publishing => "publishing",
            StreamStatus::Paused => "paused",
            StreamStatus::Error(_) => "error",
            StreamStatus::Stopped => "stopped",
        }
    }

    pub fn description(&self) -> String {
        match self {
            StreamStatus::Created => "Stream created, awaiting initialization".to_string(),
            StreamStatus::Unpublished => "Stream is ready but not publishing".to_string(),
            StreamStatus::Publishing => "Actively publishing media".to_string(),
            StreamStatus::Paused => "Stream publishing is paused".to_string(),
            StreamStatus::Error(e) => format!("Error: {}", e),
            StreamStatus::Stopped => "Stream has stopped".to_string(),
        }
    }

    pub fn is_publishing(&self) -> bool {
        matches!(self, StreamStatus::Publishing)
    }

    pub fn is_error(&self) -> bool {
        matches!(self, StreamStatus::Error(_))
    }

    pub fn is_terminated(&self) -> bool {
        matches!(self, StreamStatus::Stopped)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackStatus {
    Idle,
    Playing,
    Paused,
}

impl PlaybackStatus {
    pub fn as_str(&self) -> &str {
        match self {
            PlaybackStatus::Idle => "idle",
            PlaybackStatus::Playing => "playing",
            PlaybackStatus::Paused => "paused",
        }
    }

    pub fn description(&self) -> String {
        match self {
            PlaybackStatus::Idle => "No clients playing".to_string(),
            PlaybackStatus::Playing => "Stream is being played".to_string(),
            PlaybackStatus::Paused => "Playback is paused".to_string(),
        }
    }

    pub fn is_playing(&self) -> bool {
        matches!(self, PlaybackStatus::Playing)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamSourceMode {
    Pull,
    Push,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamProtocol {
    RTSP,
    RTMP,
    WebRTC,
    HTTP,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamSinkMode {
    Pull,
    Push,
}

impl StreamSinkMode {
    pub fn as_str(&self) -> &str {
        match self {
            StreamSinkMode::Pull => "pull",
            StreamSinkMode::Push => "push",
        }
    }

    pub fn description(&self) -> String {
        match self {
            StreamSinkMode::Pull => "Remote client pulls from server".to_string(),
            StreamSinkMode::Push => "Server pushes to remote endpoint".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReceiverStatus {
    Idle,
    Connecting,
    Playing,
    Paused,
    Error(String),
    Stopped,
}

impl ReceiverStatus {
    pub fn as_str(&self) -> &str {
        match self {
            ReceiverStatus::Idle => "idle",
            ReceiverStatus::Connecting => "connecting",
            ReceiverStatus::Playing => "playing",
            ReceiverStatus::Paused => "paused",
            ReceiverStatus::Error(_) => "error",
            ReceiverStatus::Stopped => "stopped",
        }
    }

    pub fn description(&self) -> String {
        match self {
            ReceiverStatus::Idle => "Receiver is idle".to_string(),
            ReceiverStatus::Connecting => "Connecting to stream...".to_string(),
            ReceiverStatus::Playing => "Receiving and playing stream".to_string(),
            ReceiverStatus::Paused => "Receiver is paused".to_string(),
            ReceiverStatus::Error(e) => format!("Receiver error: {}", e),
            ReceiverStatus::Stopped => "Receiver has stopped".to_string(),
        }
    }
}

pub type ReceiverId = String;

#[derive(Clone)]
pub struct StreamReceiver {
    pub id: ReceiverId,
    pub stream_id: StreamId,
    pub mode: StreamSinkMode,
    pub protocol: StreamProtocol,
    pub client_addr: Option<String>,
    pub status: ReceiverStatus,
    pub push_addr: Option<String>,
}

impl StreamReceiver {
    pub fn new(stream_id: &str, mode: StreamSinkMode, protocol: StreamProtocol) -> Self {
        Self {
            id: format!("{}_{}", stream_id, uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0")),
            stream_id: stream_id.to_string(),
            mode,
            protocol,
            client_addr: None,
            status: ReceiverStatus::Idle,
            push_addr: None,
        }
    }

    pub fn with_client_addr(mut self, addr: &str) -> Self {
        self.client_addr = Some(addr.to_string());
        self
    }

    pub fn with_push_addr(mut self, addr: &str) -> Self {
        self.push_addr = Some(addr.to_string());
        self
    }
}

#[derive(Debug, Clone)]
pub struct Track {
    pub id: TrackId,
    pub codec: CodecType,
    pub payload_type: u8,
    pub clock_rate: u32,
    pub extra_params: HashMap<String, String>,
}

impl Default for Track {
    fn default() -> Self {
        Self {
            id: 0,
            codec: CodecType::Unknown,
            payload_type: 96,
            clock_rate: 90000,
            extra_params: HashMap::new(),
        }
    }
}

impl Track {
    pub fn new(id: TrackId, codec: CodecType, payload_type: u8, clock_rate: u32) -> Self {
        Self {
            id,
            codec,
            payload_type,
            clock_rate,
            extra_params: HashMap::new(),
        }
    }

    pub fn with_extra_params(mut self, key: &str, value: &str) -> Self {
        self.extra_params.insert(key.to_string(), value.to_string());
        self
    }
}

/// Default H264 + AAC tracks for push streams without explicit SDP (e.g. RTMP publish).
pub fn default_live_tracks() -> Vec<Track> {
    vec![
        Track::new(0, CodecType::H264, 96, 90_000),
        Track::new(1, CodecType::AAC, 97, 44_100),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecType {
    H264,
    H265,
    AAC,
    Opus,
    G711,
    Unknown,
}

impl CodecType {
    pub fn from_pt(pt: u8) -> Self {
        match pt {
            0 => CodecType::G711,
            8 => CodecType::G711,
            96 => CodecType::H264,
            98 => CodecType::H265,
            97 => CodecType::AAC,
            109 => CodecType::Opus,
            _ => CodecType::Unknown,
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            CodecType::H264 => "video/H264",
            CodecType::H265 => "video/H265",
            CodecType::AAC => "audio/mp4a-latm",
            CodecType::Opus => "audio/opus",
            CodecType::G711 => "audio、PCMU",
            CodecType::Unknown => "application/RTP",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaFrame {
    pub stream_id: StreamId,
    pub track_id: TrackId,
    pub timestamp: u64,
    pub data: Bytes,
    pub is_keyframe: bool,
    pub codec: CodecType,
    pub rtp_data: Option<Bytes>,
}

impl MediaFrame {
    pub fn new(stream_id: StreamId, track_id: TrackId, timestamp: u64, data: Bytes, is_keyframe: bool, codec: CodecType) -> Self {
        Self {
            stream_id,
            track_id,
            timestamp,
            data,
            is_keyframe,
            codec,
            rtp_data: None,
        }
    }
    
    pub fn with_rtp_data(mut self, rtp_data: Bytes) -> Self {
        self.rtp_data = Some(rtp_data);
        self
    }
}

#[derive(Debug, Clone)]
pub struct RtpPacket {
    pub stream_id: StreamId,
    pub track_id: TrackId,
    pub timestamp: u32,
    pub seq: u16,
    pub payload: Bytes,
    pub is_keyframe: bool,
}

pub struct StreamManager {
    streams: RwLock<HashMap<StreamId, Stream>>,
    hubs: RwLock<HashMap<StreamId, Arc<StreamHub>>>,
    receivers: RwLock<HashMap<ReceiverId, StreamReceiver>>,
}

impl StreamManager {
    pub fn new() -> Self {
        Self {
            streams: RwLock::new(HashMap::new()),
            hubs: RwLock::new(HashMap::new()),
            receivers: RwLock::new(HashMap::new()),
        }
    }

    pub fn create_stream(&self, stream_id: &str, source: StreamSourceMode, protocol: StreamProtocol, pull_url: Option<String>) -> Stream {
        // Check if stream already exists
        {
            let streams = self.streams.read();
            if let Some(existing_stream) = streams.get(stream_id) {
                info!("[Core] Stream {} already exists, returning existing stream", stream_id);
                return existing_stream.clone();
            }
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

        // Create broadcast channel for this stream
        // let (tx, rx) = broadcast::channel(1000);
        //
        // {
        //     let mut channels = self.channels.write();
        //     channels.insert(stream_id.to_string(), tx);
        // }
        //
        // {
        //     let mut receivers = self._receivers.write();
        //     receivers.insert(stream_id.to_string(), vec![rx]);
        // }

        {
            let mut streams = self.streams.write();
            streams.insert(stream_id.to_string(), stream.clone());
        }

        stream
    }

    pub fn set_stream_tracks(&self, stream_id: &str, tracks: Vec<Track>) {
        let mut streams = self.streams.write();
        if let Some(stream) = streams.get_mut(stream_id) {
            stream.tracks = tracks;
        }
    }

    pub fn set_stream_sps_pps(&self, stream_id: &str, sps: Vec<u8>, pps: Vec<u8>) {
        let mut streams = self.streams.write();
        if let Some(stream) = streams.get_mut(stream_id) {
            if stream.sps.is_none() {
                info!("[Core] Setting SPS ({}) and PPS ({}) for stream {}", sps.len(), pps.len(), stream_id);
            }
            stream.sps = Some(sps);
            stream.pps = Some(pps);
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
        let mut streams = self.streams.write();
        if let Some(stream) = streams.get_mut(stream_id) {
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
        }
    }

    pub fn ensure_stream_hub(&self, stream_id: &str) {
        if self.hubs.read().contains_key(stream_id) {
            debug!("[Core] StreamHub already exists for stream '{}'", stream_id);
            return;
        }
        self.set_stream_hub(stream_id);
    }

    /// Back-compat alias.
    pub fn ensure_stream_broadcast(&self, stream_id: &str) {
        self.ensure_stream_hub(stream_id);
    }

    pub fn set_stream_hub(&self, stream_id: &str) {
        let streams = self.streams.read();
        if !streams.contains_key(stream_id) {
            return;
        }
        drop(streams);

        let replacing = self.hubs.read().contains_key(stream_id);
        if replacing {
            warn!(
                "[Core] Replacing StreamHub for stream '{}' (existing subscribers should re-subscribe)",
                stream_id
            );
        }
        let hub = StreamHub::new(stream_id);
        self.hubs.write().insert(stream_id.to_string(), hub);
        info!("[Core] StreamHub ready for stream '{}'", stream_id);
    }

    /// Back-compat alias.
    pub fn set_stream_broadcast(&self, stream_id: &str) {
        self.set_stream_hub(stream_id);
    }

    pub fn get_hub(&self, stream_id: &str) -> Option<Arc<StreamHub>> {
        self.hubs.read().get(stream_id).cloned()
    }

    pub fn remove_stream(&self, stream_id: &StreamId) -> Option<Stream> {
        self.hubs.write().remove(stream_id);
        let mut streams = self.streams.write();
        streams.remove(stream_id)
    }

    pub fn get_stream(&self, stream_id: &StreamId) -> Option<Stream> {
        let streams = self.streams.read();
        streams.get(stream_id).cloned()
    }

    pub fn list_streams(&self) -> Vec<StreamId> {
        let streams = self.streams.read();
        streams.keys().cloned().collect()
    }

    pub fn set_status(&self, stream_id: &str, status: StreamStatus) -> Result<()> {
        let mut streams = self.streams.write();
        if let Some(stream) = streams.get_mut(stream_id) {
            let old_status = stream.status.clone();
            stream.status = status.clone();
            info!("[Core] Stream {} status changed from {:?} to {:?}", stream_id, old_status, status);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Stream {} not found", stream_id))
        }
    }

    pub fn set_playback_status(&self, stream_id: &str, status: PlaybackStatus) -> Result<()> {
        let mut streams = self.streams.write();
        if let Some(stream) = streams.get_mut(stream_id) {
            let old_status = stream.playback_status.clone();
            stream.playback_status = status.clone();
            info!("[Core] Stream {} playback status changed from {:?} to {:?}", stream_id, old_status, status);
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

    pub fn create_receiver(&self, stream_id: &str, mode: StreamSinkMode, protocol: StreamProtocol) -> StreamReceiver {
        let receiver = StreamReceiver::new(stream_id, mode, protocol);
        let receiver_id = receiver.id.clone();
        
        {
            let mut receivers = self.receivers.write();
            receivers.insert(receiver_id, receiver.clone());
        }
        
        info!("[Core] Created receiver {} for stream {}", receiver.id, stream_id);
        receiver
    }

    pub fn create_pull_receiver(&self, stream_id: &str, protocol: StreamProtocol, client_addr: &str) -> StreamReceiver {
        let receiver = StreamReceiver::new(stream_id, StreamSinkMode::Pull, protocol)
            .with_client_addr(client_addr);
        
        let receiver_id = receiver.id.clone();
        {
            let mut receivers = self.receivers.write();
            receivers.insert(receiver_id, receiver.clone());
        }
        
        info!("[Core] Created pull receiver {} for stream {} from client {}", receiver.id, stream_id, client_addr);
        receiver
    }

    pub fn create_push_receiver(&self, stream_id: &str, protocol: StreamProtocol, push_addr: &str) -> StreamReceiver {
        let receiver = StreamReceiver::new(stream_id, StreamSinkMode::Push, protocol)
            .with_push_addr(push_addr);
        
        let receiver_id = receiver.id.clone();
        {
            let mut receivers = self.receivers.write();
            receivers.insert(receiver_id, receiver.clone());
        }
        
        info!("[Core] Created push receiver {} for stream {} to {}", receiver.id, stream_id, push_addr);
        receiver
    }

    pub fn remove_receiver(&self, receiver_id: &ReceiverId) -> Option<StreamReceiver> {
        let mut receivers = self.receivers.write();
        let receiver = receivers.remove(receiver_id);
        
        if let Some(r) = &receiver {
            info!("[Core] Removed receiver {} for stream {}", r.id, r.stream_id);
        }
        
        receiver
    }

    pub fn get_receiver(&self, receiver_id: &ReceiverId) -> Option<StreamReceiver> {
        let receivers = self.receivers.read();
        receivers.get(receiver_id).cloned()
    }

    pub fn list_receivers(&self) -> Vec<ReceiverId> {
        let receivers = self.receivers.read();
        receivers.keys().cloned().collect()
    }

    pub fn list_receivers_for_stream(&self, stream_id: &StreamId) -> Vec<StreamReceiver> {
        let receivers = self.receivers.read();
        receivers.values()
            .filter(|r| r.stream_id == *stream_id)
            .cloned()
            .collect()
    }

    pub fn set_receiver_status(&self, receiver_id: &ReceiverId, status: ReceiverStatus) -> Result<()> {
        let mut receivers = self.receivers.write();
        if let Some(receiver) = receivers.get_mut(receiver_id) {
            let old_status = receiver.status.clone();
            receiver.status = status.clone();
            info!("[Core] Receiver {} status changed from {:?} to {:?}", receiver_id, old_status, status);
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
        if self.get_hub(stream_id).is_none() {
            self.ensure_stream_hub(stream_id);
        }
        let hub = self.get_hub(stream_id)?;
        info!("[Core] dispatch_subscribe: stream_id={} policy={:?}", stream_id, policy);
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

        if let Some(hub) = self.get_hub(&stream_id) {
            let seq = hub.publish(frame);
            debug!("[Core] publish_frame: stream_id={} ring_seq={}", stream_id, seq);
        } else {
            self.ensure_stream_hub(&stream_id);
            if let Some(hub) = self.get_hub(&stream_id) {
                let seq = hub.publish(frame);
                debug!("[Core] publish_frame: stream_id={} ring_seq={}", stream_id, seq);
            } else {
                warn!("[Core] publish_frame: No StreamHub for stream_id={}", stream_id);
            }
        }

        let streams_read = self.streams.read();
        if let Some(stream) = streams_read.get(&stream_id) {
            match stream.status {
                StreamStatus::Created | StreamStatus::Unpublished => {
                    drop(streams_read);
                    let mut streams = self.streams.write();
                    if let Some(s) = streams.get_mut(&stream_id) {
                        s.status = StreamStatus::Publishing;
                        debug!("[Core] Stream {} status -> Publishing", stream_id);
                    }
                }
                _ => {}
            }
        }
    }

    pub fn update_stream_status(&self, stream_id: &StreamId, status: StreamStatus) {
        let mut streams = self.streams.write();
        if let Some(stream) = streams.get_mut(stream_id) {
            stream.status = status;
        }
    }
}

impl Default for StreamManager {
    fn default() -> Self {
        Self::new()
    }
}
