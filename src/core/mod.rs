mod config;
mod tester;
mod pusher;
mod timestamp;
mod broadcast_edge;
pub mod protocol;

pub use broadcast_edge::{drain_broadcast_lag, is_playable_video, recv_coalesced_video, recv_flv_batch};

use std::collections::HashMap;
use parking_lot::RwLock;
use bytes::Bytes;
use tokio::sync::broadcast;
use tracing::{info, warn, error, debug};
use anyhow::Result;

pub use config::{Config, RtmpConfig, RtspConfig, WebrtcConfig, HttpConfig, StreamConfig, TrackConfig};
pub use tester::StreamTester;
pub use pusher::*;
pub use protocol::{ProtocolType, ProtocolInfo, ProtocolRegistry, StreamSink};
pub use timestamp::{flv_timestamp_ms, media_timestamp_delta_ms};

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
    /// Most recent keyframe (Annex B), for late WebRTC play subscribers.
    pub last_keyframe: Option<Vec<u8>>,
    pub last_keyframe_ts: Option<u64>,
    /// Frames since the last keyframe (inclusive), for late WebRTC play join.
    pub gop_frames: Vec<MediaFrame>,
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
    Error(String),
    Stopped,
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
    channels: RwLock<HashMap<StreamId, broadcast::Sender<MediaFrame>>>,
    receivers: RwLock<HashMap<ReceiverId, StreamReceiver>>,
}

impl StreamManager {
    pub fn new() -> Self {
        Self {
            streams: RwLock::new(HashMap::new()),
            channels: RwLock::new(HashMap::new()),
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
            last_keyframe: None,
            last_keyframe_ts: None,
            gop_frames: Vec::new(),
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

    pub fn ensure_stream_broadcast(&self, stream_id: &str) {
        if self.channels.read().contains_key(stream_id) {
            debug!("[Core] Broadcast channel already exists for stream '{}'", stream_id);
            return;
        }
        self.set_stream_broadcast(stream_id);
    }

    pub fn set_stream_broadcast(&self, stream_id: &str) {
        let mut streams = self.streams.write();
        if let Some(stream) = streams.get_mut(stream_id) {
            let replacing = {
                let channels = self.channels.read();
                channels.contains_key(stream_id)
            };
            if replacing {
                warn!(
                    "[Core] Replacing broadcast channel for stream '{}' (existing subscribers will stop receiving)",
                    stream_id
                );
            }
            let (tx, _rx) = broadcast::channel(2048);

            {
                let mut channels = self.channels.write();
                channels.insert(stream.id.clone(), tx);
            }
            info!("[Core] Broadcast channel ready for stream '{}'", stream_id);
        }
    }

    pub fn remove_stream(&self, stream_id: &StreamId) -> Option<Stream> {
        {
            let mut channels = self.channels.write();
            channels.remove(stream_id);
        }
        
        // {
        //     let mut receivers = self._receivers.write();
        //     receivers.remove(stream_id);
        // }

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
        self.streams.read().get(stream_id).and_then(|s| {
            Some((s.last_keyframe.clone()?, s.last_keyframe_ts?))
        })
    }

    /// Snapshot of frames from the last keyframe onward (for WebRTC play catch-up).
    pub fn get_gop_frames(&self, stream_id: &str) -> Vec<MediaFrame> {
        self.streams
            .read()
            .get(stream_id)
            .map(|s| s.gop_frames.clone())
            .unwrap_or_default()
    }

    /// Recent GOP tail for play catch-up (from last keyframe, capped).
    /// Returns empty if the tail is too long — caller waits for a fresh IDR instead.
    pub fn get_recent_gop_for_play(&self, stream_id: &str, max_frames: usize) -> Vec<MediaFrame> {
        let gop = self.get_gop_frames(stream_id);
        if gop.is_empty() || max_frames == 0 {
            return Vec::new();
        }
        let start = gop
            .iter()
            .rposition(|f| f.is_keyframe)
            .unwrap_or(0);
        let tail = &gop[start..];
        if tail.len() <= max_frames {
            tail.to_vec()
        } else {
            Vec::new()
        }
    }

    fn update_gop_buffer(stream: &mut Stream, frame: &MediaFrame) {
        if !matches!(frame.codec, CodecType::H264 | CodecType::H265) || frame.track_id != 0 {
            return;
        }
        if frame.is_keyframe {
            stream.gop_frames.clear();
        }
        stream.gop_frames.push(frame.clone());
        // Never drop frames in the middle of a GOP — that breaks H264 references.
        // With periodic IDR from the publisher, each GOP stays small.
    }

    pub fn publish_frame(&self, frame: MediaFrame) {
        let stream_id = frame.stream_id.clone();
        debug!("[Core] publish_frame: stream_id={}, track_id={}, timestamp={}, is_keyframe={}, codec={}, data_len={}", 
              stream_id, frame.track_id, frame.timestamp, frame.is_keyframe, frame.codec as u8, frame.data.len());

        {
            let mut streams = self.streams.write();
            if let Some(stream) = streams.get_mut(&stream_id) {
                if frame.is_keyframe && matches!(frame.codec, CodecType::H264 | CodecType::H265) {
                    stream.last_keyframe = Some(frame.data.to_vec());
                    stream.last_keyframe_ts = Some(frame.timestamp);
                    info!(
                        "[Core] Keyframe stream='{}' ts={} gop_len={}",
                        stream_id,
                        frame.timestamp,
                        stream.gop_frames.len()
                    );
                }
                Self::update_gop_buffer(stream, &frame);
            }
        }
        
        let channels = self.channels.read();
        let tx_option = channels.get(&stream_id).cloned();
        
        if let Some(tx) = tx_option {
            match tx.send(frame) {
                Ok(lagged) => {
                    let subscribers = tx.receiver_count();
                    if subscribers > 0 {
                        debug!(
                            "[Core] publish_frame: stream_id={} subscribers={} lagged={}",
                            stream_id, subscribers, lagged
                        );
                    }
                }
                Err(e) => {
                    debug!(
                        "[Core] publish_frame: no active receivers stream_id={} ({})",
                        stream_id, e
                    );
                }
            }
        } else {
            warn!("[Core] publish_frame: No broadcast channel for stream_id={}", stream_id);
        }

        // Update stream status - transition to Publishing when receiving frames
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

    pub fn subscribe(&self, stream_id: &StreamId) -> Option<broadcast::Receiver<MediaFrame>> {
        let channels = self.channels.read();
        if let Some(tx) = channels.get(stream_id) {
            let receivers_before = tx.receiver_count();
            let rx = tx.subscribe();
            let receivers_after = tx.receiver_count();
            info!("[Core] subscribe: stream_id={}, receivers_before={}, receivers_after={}", 
                  stream_id, receivers_before, receivers_after);
            return Some(rx)
        } else {
            warn!("[Core] subscribe: No broadcast channel found for stream_id={}", stream_id);
            // Create broadcast channel for this stream
            let (tx, rx) = broadcast::channel(2048);

            {
                let mut channels = self.channels.write();
                channels.insert(stream_id.to_string(), tx);
            }

            return Some(rx)
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
