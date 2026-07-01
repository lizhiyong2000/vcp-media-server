/// RTMP session state management
/// Tracks connection state, stream state, and per-session configuration.
use std::collections::HashMap;
use bytes::Bytes;
use tracing::{info, debug, Level};

use crate::core::{CodecType, MediaFrame, StreamSourceMode, StreamProtocol};
use super::amf0::{self, Amf0Value};

/// RTMP session state
#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
    /// Initial state after TCP connect
    Handshaking,
    /// Handshake complete, waiting for connect command
    Connected,
    /// Connect acknowledged, waiting for createStream
    CreateStream,
    /// Stream created, ready for publish/play
    Ready,
    /// Actively publishing media
    Publishing,
    /// Actively playing media
    Playing,
    /// Session closing
    Closing,
}

/// RTMP session for a single client connection
pub struct RtmpSession {
    /// Current session state
    pub state: SessionState,
    /// Application name (from connect command)
    pub app: String,
    /// Stream name (from publish/play command)
    pub stream_name: String,
    /// Stream key (for publish, if applicable)
    pub stream_key: String,
    /// Whether this is a publish or play session
    pub mode: SessionMode,
    /// Negotiated chunk size (our side)
    pub chunk_size: usize,
    /// Client's chunk size
    pub client_chunk_size: usize,
    /// Server-side stream ID (incremented on createStream)
    pub server_stream_id: u32,
    /// Whether we've sent the AVC sequence header (SPS/PPS)
    pub avc_header_sent: bool,
    /// Whether we've sent the AAC sequence header
    pub aac_header_sent: bool,
    /// Whether we've sent onMetaData
    pub metadata_sent: bool,
    /// Transaction ID counter for commands
    pub transaction_id: f64,
    /// Peer address string for logging
    pub peer_addr: String,
}

/// Session mode: publish or play
#[derive(Debug, Clone, PartialEq)]
pub enum SessionMode {
    None,
    Publish,
    Play,
}

impl RtmpSession {
    pub fn new(peer_addr: &str) -> Self {
        Self {
            state: SessionState::Handshaking,
            app: String::new(),
            stream_name: String::new(),
            stream_key: String::new(),
            mode: SessionMode::None,
            chunk_size: 4096,
            client_chunk_size: 128,
            server_stream_id: 0,
            avc_header_sent: false,
            aac_header_sent: false,
            metadata_sent: false,
            transaction_id: 1.0,
            peer_addr: peer_addr.to_string(),
        }
    }

    /// Handle a connect command, extract app name
    pub fn handle_connect(&mut self, args: &[Amf0Value]) -> String {
        // Extract app from command args
        for arg in args {
            if let Some(map) = arg.as_object() {
                if let Some(app) = map.get("app").and_then(|v| v.as_str()) {
                    self.app = app.to_string();
                    // info!("[RTMP] [{}] App: {}", self.peer_addr, self.app);
                }
            }
        }
        self.state = SessionState::Connected;
        // self.transaction_id += 1.0;
        self.app.clone()
    }

    /// Handle createStream command
    pub fn handle_create_stream(&mut self, args: &[Amf0Value]) -> u32 {
        self.server_stream_id += 1;
        self.state = SessionState::Ready;
        self.transaction_id = args[0].as_f64().unwrap();
        // debug!("[RTMP] [{}] Created server stream id={}, args: {:?}", self.peer_addr, self.server_stream_id, args);
        self.server_stream_id
    }

    /// Handle publish command
    pub fn handle_publish(&mut self, args: &[Amf0Value]) -> String {
        for arg in args {
            if let Some(s) = arg.as_str() {
                if s.len()> 0{
                    self.stream_name = s.to_string();
                    break;
                }

            }
            if let Some(map) = arg.as_object() {
                if let Some(name) = map.get("streamName").and_then(|v| v.as_str()) {
                    self.stream_name = name.to_string();
                }
            }
        }
        self.mode = SessionMode::Publish;
        self.state = SessionState::Publishing;
        info!("[RTMP] [{}] Publishing to stream: {}", self.peer_addr, self.stream_name);
        self.stream_name.clone()
    }

    /// Handle play command
    pub fn handle_play(&mut self, args: &[Amf0Value]) -> String {
        for arg in args {
            if let Some(s) = arg.as_str() {
                if s.len()> 0{
                    self.stream_name = s.to_string();
                    break;
                }
            }
            if let Some(map) = arg.as_object() {
                if let Some(name) = map.get("streamName").and_then(|v| v.as_str()) {
                    self.stream_name = name.to_string();
                }
            }
        }
        self.mode = SessionMode::Play;
        self.state = SessionState::Playing;
        debug!("[RTMP] [{}] Playing stream: {}", self.peer_addr, self.stream_name);
        self.stream_name.clone()
    }

    /// Handle SetChunkSize
    pub fn handle_set_chunk_size(&mut self, payload: &[u8]) {
        if payload.len() >= 4 {
            let new_size = ((payload[0] as u32) << 24)
                | ((payload[1] as u32) << 16)
                | ((payload[2] as u32) << 8)
                | (payload[3] as u32);
            self.client_chunk_size = new_size as usize;
            info!("[RTMP] [{}] Client chunk size: {}", self.peer_addr, self.client_chunk_size);
        }
    }

    /// Next transaction ID
    pub fn next_transaction_id(&mut self) -> f64 {
        let id = self.transaction_id;
        self.transaction_id += 1.0;
        id
    }
}

/// Build _result response for connect/createStream
pub fn build_result_response(transaction_id: f64, command: &str) -> Vec<u8> {
    let mut values = vec![
        Amf0Value::String("_result".to_string()), // Response command should be _result
        Amf0Value::Number(transaction_id),
    ];

    // Properties object
    let mut props = HashMap::new();
    props.insert("fmsVer".to_string(), Amf0Value::String("FMS/3.0.1.123".to_string()));
    props.insert("capabilities".to_string(), Amf0Value::Number(31.0));
    props.insert("objectEncoding".to_string(), Amf0Value::Number(0.0)); // AMF0 encoding
    values.push(Amf0Value::Object(props));

    // Information object
    let mut info = HashMap::new();
    info.insert("level".to_string(), Amf0Value::String("status".to_string()));
    info.insert("code".to_string(), Amf0Value::String(format!("NetConnection.{}.Success", command)));
    info.insert("description".to_string(), Amf0Value::String(format!("{} succeeded.", command)));
    info.insert("data".to_string(), Amf0Value::Object(HashMap::new())); // Add data object
    values.push(Amf0Value::Object(info));

    amf0::encode(&values)
}

/// Build onStatus response for publish/play
pub fn build_on_status(code: &str, description: &str, level: &str) -> Vec<u8> {
    let mut values = vec![
        Amf0Value::String("onStatus".to_string()),
        Amf0Value::Number(0.0),
        Amf0Value::Null,
    ];

    let mut info = HashMap::new();
    info.insert("level".to_string(), Amf0Value::String(level.to_string()));
    info.insert("code".to_string(), Amf0Value::String(code.to_string()));
    info.insert("description".to_string(), Amf0Value::String(description.to_string()));
    values.push(Amf0Value::Object(info));

    amf0::encode(&values)
}

/// Build AVC sequence header (SPS/PPS) as RTMP video message payload
pub fn build_avc_sequence_header(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    // Frame type + codec: keyframe(1) + AVC(7) = 0x17
    data.push(0x17);
    // AVC packet type: sequence header = 0x00
    data.push(0x00);
    // Composition time (3 bytes)
    data.extend_from_slice(&[0x00, 0x00, 0x00]);

    // AVCDecoderConfigurationRecord
    data.push(0x01); // configurationVersion
    if sps.len() >= 4 {
        data.push(sps[1]); // AVCProfileIndication
        data.push(sps[2]); // profile_compatibility
        data.push(sps[3]); // AVCLevelIndication
    } else {
        data.push(0x42);
        data.push(0x00);
        data.push(0x1F);
    }
    data.push(0xFF); // lengthSizeMinusOne = 3
    data.push(0xE1); // numOfSPS = 1

    // SPS
    let sps_len = sps.len() as u16;
    data.push((sps_len >> 8) as u8);
    data.push((sps_len & 0xFF) as u8);
    data.extend_from_slice(sps);

    // PPS
    data.push(0x01); // numOfPPS = 1
    let pps_len = pps.len() as u16;
    data.push((pps_len >> 8) as u8);
    data.push((pps_len & 0xFF) as u8);
    data.extend_from_slice(pps);

    data
}

/// Build AAC sequence header as RTMP audio message payload
pub fn build_aac_sequence_header() -> Vec<u8> {
    let mut data = Vec::new();
    data.push(0xAF); // AAC 44kHz 16bit stereo
    data.push(0x00); // Sequence header
    // AudioSpecificConfig: AAC-LC, 44100Hz, stereo
    data.push(0x12);
    data.push(0x10);
    data
}

/// Build onMetaData script message
pub fn build_metadata(width: u32, height: u32, stream_name: &str) -> Vec<u8> {
    let mut values = vec![
        Amf0Value::String("onMetaData".to_string()),
    ];

    let mut meta = HashMap::new();
    meta.insert("width".to_string(), Amf0Value::Number(width as f64));
    meta.insert("height".to_string(), Amf0Value::Number(height as f64));
    meta.insert("streamName".to_string(), Amf0Value::String(stream_name.to_string()));
    meta.insert("encoder".to_string(), Amf0Value::String("Rust-Media-Server".to_string()));
    values.push(Amf0Value::EcmaArray(meta));

    amf0::encode(&values)
}

/// Convert a MediaFrame to RTMP video data
/// frame.data is in Annex B format: [00 00 00 01][NALU][00 00 00 01][NALU]...
/// Output is in AVCC format: [frame_type|codec][avc_packet_type][composition_time][length(4B)][NALU]...
pub fn frame_to_rtmp_video(frame: &MediaFrame) -> Vec<u8> {
    let mut data = Vec::new();

    // Frame type + codec
    let frame_type = if frame.is_keyframe { 0x10 } else { 0x20 };
    data.push(frame_type | 0x07); // AVC
    // AVC packet type: NALU = 0x01
    data.push(0x01);
    // Composition time offset (3 bytes)
    data.extend_from_slice(&[0x00, 0x00, 0x00]);

    // Convert Annex B to AVCC: find start codes and write length-prefixed NALUs
    let annex_b = &frame.data;
    let mut i = 0;
    while i < annex_b.len() {
        // Find next start code (0x000001 or 0x00000001)
        let start = if i + 3 <= annex_b.len() && annex_b[i..i+3] == [0x00, 0x00, 0x01] {
            i += 3;
            i
        } else if i + 4 <= annex_b.len() && annex_b[i..i+4] == [0x00, 0x00, 0x00, 0x01] {
            i += 4;
            i
        } else {
            i += 1;
            continue;
        };

        // Find end of this NALU (next start code or end of data)
        let mut end = annex_b.len();
        let mut j = start;
        while j + 3 <= annex_b.len() {
            if (j + 3 <= annex_b.len() && annex_b[j..j+3] == [0x00, 0x00, 0x01])
                || (j + 4 <= annex_b.len() && annex_b[j..j+4] == [0x00, 0x00, 0x00, 0x01])
            {
                end = j;
                break;
            }
            j += 1;
        }

        // Write NALU with 4-byte length prefix (AVCC format)
        let nalu_len = (end - start) as u32;
        data.extend_from_slice(&nalu_len.to_be_bytes());
        data.extend_from_slice(&annex_b[start..end]);
        i = end;
    }

    data
}

/// Convert a MediaFrame to RTMP audio data
pub fn frame_to_rtmp_audio(frame: &MediaFrame) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(0xAF); // AAC 44kHz 16bit stereo
    data.push(0x01); // AAC raw
    data.extend_from_slice(&frame.data);
    data
}

/// Map publisher timestamps (RTMP ms or RTP 90 kHz) to a session-local RTMP timeline.
#[derive(Default)]
pub struct RtmpPlayClock(crate::core::FlvPlayTimeline);

impl RtmpPlayClock {
    pub fn map(&mut self, frame: &MediaFrame) -> u32 {
        self.0.map(frame)
    }
}
