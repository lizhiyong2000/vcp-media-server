/// HTTP-FLV streaming module
/// Delivers live streams via HTTP with FLV container format using chunked transfer encoding.
use std::collections::HashMap;
use std::sync::Arc;
use bytes::{Bytes, BytesMut, BufMut};
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{info, warn, error, debug};
use anyhow::Result;

use crate::core::{StreamManager, MediaFrame, CodecType};
use crate::rtmp::session::{frame_to_rtmp_audio, frame_to_rtmp_video};

/// FLV file header (9 bytes)
fn generate_flv_header(has_video: bool, has_audio: bool) -> Vec<u8> {
    let mut header = Vec::with_capacity(13);
    // Signature: "FLV"
    header.extend_from_slice(b"FLV");
    // Version: 1
    header.push(0x01);
    // Flags: audio/video
    let mut flags: u8 = 0;
    if has_audio {
        flags |= 0x04;
    }
    if has_video {
        flags |= 0x01;
    }
    header.push(flags);
    // Data offset: 9 (header size)
    header.extend_from_slice(&[0x00, 0x00, 0x00, 0x09]);
    // Previous tag size 0 (4 bytes)
    header.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    header
}

/// Generate an FLV tag
fn generate_flv_tag(tag_type: u8, timestamp: u32, data: &[u8]) -> Vec<u8> {
    let data_size = data.len() as u32;
    let mut tag = Vec::with_capacity(11 + data.len() + 4);

    // Tag type (1=audio, 9=video, 18=script)
    tag.push(tag_type);
    // Data size (3 bytes)
    tag.push(((data_size >> 16) & 0xFF) as u8);
    tag.push(((data_size >> 8) & 0xFF) as u8);
    tag.push((data_size & 0xFF) as u8);
    // Timestamp (3 bytes + 1 extension)
    tag.push(((timestamp >> 16) & 0xFF) as u8);
    tag.push(((timestamp >> 8) & 0xFF) as u8);
    tag.push((timestamp & 0xFF) as u8);
    tag.push(((timestamp >> 24) & 0xFF) as u8); // Timestamp extension
    // Stream ID (always 0)
    tag.extend_from_slice(&[0x00, 0x00, 0x00]);
    // Tag data
    tag.extend_from_slice(data);
    // Previous tag size (4 bytes)
    let total_size = (11 + data_size) as u32;
    tag.extend_from_slice(&total_size.to_be_bytes());

    tag
}

/// Generate AVC sequence header (SPS/PPS) for video
fn generate_avc_sequence_header(sps: &[u8], pps: &[u8]) -> Vec<u8> {
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
        data.push(0x42); // Baseline
        data.push(0x00);
        data.push(0x1F);
    }
    data.push(0xFF); // lengthSizeMinusOne = 3 (4 bytes NAL length)
    data.push(0xE1); // numOfSequenceParameterSets = 1

    // SPS
    let sps_len = sps.len() as u16;
    data.push((sps_len >> 8) as u8);
    data.push((sps_len & 0xFF) as u8);
    data.extend_from_slice(sps);

    // PPS
    data.push(0x01); // numOfPictureParameterSets = 1
    let pps_len = pps.len() as u16;
    data.push((pps_len >> 8) as u8);
    data.push((pps_len & 0xFF) as u8);
    data.extend_from_slice(pps);

    // Wrap in FLV tag
    generate_flv_tag(0x09, 0, &data)
}

/// Generate AAC sequence header
fn generate_aac_sequence_header() -> Vec<u8> {
    let mut data = Vec::new();
    // Sound format + rate + size + type: AAC(10) = 0xAF
    data.push(0xAF);
    // AAC packet type: sequence header = 0x00
    data.push(0x00);
    // AudioSpecificConfig: AAC-LC, 44100Hz, mono
    // 0x1210 = AAC-LC 44100Hz stereo
    data.push(0x12);
    data.push(0x10);

    generate_flv_tag(0x08, 0, &data)
}

/// Generate onMetaData script tag
fn generate_metadata_tag(stream_id: &str, has_video: bool, has_audio: bool) -> Vec<u8> {
    let mut data = Vec::new();

    // AMF0 string: "onMetaData"
    data.push(0x02); // String type
    let name = "onMetaData";
    data.extend_from_slice(&(name.len() as u16).to_be_bytes());
    data.extend_from_slice(name.as_bytes());

    // ECMA array
    data.push(0x08); // ECMA array type
    let prop_count = 3u32;
    data.extend_from_slice(&prop_count.to_be_bytes());

    // Property: width
    let key = "width";
    data.extend_from_slice(&(key.len() as u16).to_be_bytes());
    data.extend_from_slice(key.as_bytes());
    data.push(0x00); // Number type
    data.extend_from_slice(&0u64.to_be_bytes()); // placeholder

    // Property: height
    let key = "height";
    data.extend_from_slice(&(key.len() as u16).to_be_bytes());
    data.extend_from_slice(key.as_bytes());
    data.push(0x00);
    data.extend_from_slice(&0u64.to_be_bytes());

    // Property: streamName
    let key = "streamName";
    data.extend_from_slice(&(key.len() as u16).to_be_bytes());
    data.extend_from_slice(key.as_bytes());
    data.push(0x02); // String type
    data.extend_from_slice(&(stream_id.len() as u16).to_be_bytes());
    data.extend_from_slice(stream_id.as_bytes());

    // End marker
    data.push(0x00);
    data.push(0x00);
    data.push(0x09);

    generate_flv_tag(0x12, 0, &data)
}

/// Convert a MediaFrame to FLV video tag data (Annex B → AVCC, same as RTMP play path).
fn frame_to_flv_video(frame: &MediaFrame) -> Vec<u8> {
    let data = frame_to_rtmp_video(frame);
    if data.is_empty() {
        return Vec::new();
    }
    let timestamp = (frame.timestamp & 0xFFFFFFFF) as u32;
    generate_flv_tag(0x09, timestamp, &data)
}

/// Convert a MediaFrame to FLV audio tag data
fn frame_to_flv_audio(frame: &MediaFrame) -> Vec<u8> {
    let data = frame_to_rtmp_audio(frame);
    let timestamp = (frame.timestamp & 0xFFFFFFFF) as u32;
    generate_flv_tag(0x08, timestamp, &data)
}

/// HTTP-FLV session for a single client
pub struct HttpFlvSession {
    stream_id: String,
    header_sent: bool,
    metadata_sent: bool,
    sequence_header_sent: bool,
}

impl HttpFlvSession {
    pub fn new(stream_id: &str) -> Self {
        Self {
            stream_id: stream_id.to_string(),
            header_sent: false,
            metadata_sent: false,
            sequence_header_sent: false,
        }
    }

    /// Generate the initial HTTP response headers for FLV streaming
    pub fn generate_http_headers() -> String {
        let mut headers = String::new();
        headers.push_str("HTTP/1.1 200 OK\r\n");
        headers.push_str("Content-Type: video/x-flv\r\n");
        headers.push_str("Transfer-Encoding: chunked\r\n");
        headers.push_str("Connection: close\r\n");
        headers.push_str("Access-Control-Allow-Origin: *\r\n");
        headers.push_str("Cache-Control: no-cache\r\n");
        headers.push_str("\r\n");
        headers
    }

    /// Whether AVC/AAC sequence headers still need to be sent
    pub fn needs_sequence_headers(&self) -> bool {
        !self.sequence_header_sent
    }

    /// Generate the initial FLV data (header + metadata + sequence headers)
    pub fn generate_initial_data(&mut self, stream: &crate::core::Stream) -> Vec<u8> {
        let mut data = Vec::new();

        // FLV header
        if !self.header_sent {
            data.extend(generate_flv_header(true, true));
            self.header_sent = true;
        }

        // Metadata
        if !self.metadata_sent {
            data.extend(generate_metadata_tag(&self.stream_id, true, true));
            self.metadata_sent = true;
        }

        // AVC sequence header (SPS/PPS)
        if !self.sequence_header_sent {
            if let (Some(ref sps), Some(ref pps)) = (&stream.sps, &stream.pps) {
                data.extend(generate_avc_sequence_header(sps, pps));
                data.extend(generate_aac_sequence_header());
                self.sequence_header_sent = true;
            }
        }

        data
    }

    /// Convert a media frame to FLV tag
    pub fn frame_to_flv(&self, frame: &MediaFrame) -> Vec<u8> {
        match frame.codec {
            CodecType::H264 | CodecType::H265 => frame_to_flv_video(frame),
            CodecType::AAC | CodecType::Opus | CodecType::G711 => frame_to_flv_audio(frame),
            _ => Vec::new(),
        }
    }
}

/// Format data as HTTP chunk
pub fn format_chunk(data: &[u8]) -> Vec<u8> {
    let mut chunk = Vec::with_capacity(data.len() + 16);
    chunk.extend_from_slice(format!("{:x}\r\n", data.len()).as_bytes());
    chunk.extend_from_slice(data);
    chunk.extend_from_slice(b"\r\n");
    chunk
}

/// HTTP-FLV streaming handler
pub struct HttpFlvServer {
    stream_manager: Arc<StreamManager>,
}

impl HttpFlvServer {
    pub fn new(stream_manager: Arc<StreamManager>) -> Self {
        Self { stream_manager }
    }

    /// Check if a stream exists
    pub fn has_stream(&self, stream_id: &str) -> bool {
        self.stream_manager.get_stream(&stream_id.to_string()).is_some()
    }

    /// Get the stream manager reference
    pub fn stream_manager(&self) -> &Arc<StreamManager> {
        &self.stream_manager
    }

    /// Create a new FLV session for a client
    pub fn create_session(&self, stream_id: &str) -> Option<(HttpFlvSession, crate::core::Stream)> {
        let stream = self.stream_manager.get_stream(&stream_id.to_string())?;
        let session = HttpFlvSession::new(stream_id);
        Some((session, stream))
    }

    /// Subscribe to a stream's broadcast channel
    pub fn subscribe(&self, stream_id: &str) -> Option<broadcast::Receiver<MediaFrame>> {
        self.stream_manager.subscribe(&stream_id.to_string())
    }
}
