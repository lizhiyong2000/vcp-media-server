mod config;
pub mod dispatch;
mod frame_ring;
pub mod live_play;
pub mod protocol;
mod pusher;
mod stream_hub;
mod stream_manager;
mod tester;
mod timestamp;

pub use dispatch::{coalesce_flv_batch, DispatchError, DispatchPolicy, DispatchReader};
pub use frame_ring::{is_playable_video, is_video_keyframe, FrameRing, SnapMode};
pub use stream_hub::StreamHub;
pub use stream_manager::StreamManager;

use bytes::Bytes;
use std::collections::HashMap;

pub use config::{
    AnalysisConfig, Config, HttpConfig, RecordConfig, RtmpConfig, RtspConfig, SnapshotConfig,
    StreamConfig, TrackConfig, WebrtcConfig,
};
pub use live_play::{
    is_idr_frame, is_playable_video_frame, prime_live_play, recv_coalesced_play_frame,
};
pub use protocol::{ProtocolInfo, ProtocolRegistry, ProtocolType, StreamSink};
pub use pusher::*;
pub use tester::StreamTester;
pub use timestamp::{
    flv_timestamp_ms, media_frame_timestamp_delta_ms, media_timestamp_delta_ms,
    media_timestamp_delta_ms_with_clock, FlvPlayTimeline, WallclockMsTimeline,
    AAC_DEFAULT_CLOCK_RATE, MILLISECOND_CLOCK_RATE, VIDEO_RTP_CLOCK_RATE,
};

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
            id: format!(
                "{}_{}",
                stream_id,
                uuid::Uuid::new_v4()
                    .to_string()
                    .split('-')
                    .next()
                    .unwrap_or("0")
            ),
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
    pub clock_rate: Option<u32>,
    pub data: Bytes,
    pub is_keyframe: bool,
    pub codec: CodecType,
    pub rtp_data: Option<Bytes>,
}

impl MediaFrame {
    pub fn new(
        stream_id: StreamId,
        track_id: TrackId,
        timestamp: u64,
        data: Bytes,
        is_keyframe: bool,
        codec: CodecType,
    ) -> Self {
        Self {
            stream_id,
            track_id,
            timestamp,
            clock_rate: None,
            data,
            is_keyframe,
            codec,
            rtp_data: None,
        }
    }

    pub fn with_clock_rate(mut self, clock_rate: u32) -> Self {
        self.clock_rate = Some(clock_rate);
        self
    }

    pub fn with_optional_clock_rate(mut self, clock_rate: Option<u32>) -> Self {
        self.clock_rate = clock_rate;
        self
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
