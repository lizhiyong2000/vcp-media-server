//! RTMP test helper functions.
//!
//! Provides utilities for RTMP protocol testing.

use super::TestCodec;
use bytes::BytesMut;

/// RTMP test helper structure.
#[derive(Debug, Clone)]
pub struct RtmpTestHelper {
    stream_id: String,
    app: String,
    host: String,
    port: u16,
}

impl RtmpTestHelper {
    /// Create a new RTMP test helper.
    pub fn new(stream_id: &str, host: &str, port: u16) -> Self {
        Self {
            stream_id: stream_id.to_string(),
            app: "live".to_string(),
            host: host.to_string(),
            port,
        }
    }

    /// Get the stream ID.
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Get the server host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Get the server port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Generate RTMP publish command.
    pub fn publish_command(&self) -> String {
        create_rtmp_publish_command(&self.stream_id, &self.app)
    }

    /// Generate RTMP play command.
    pub fn play_command(&self) -> String {
        create_rtmp_play_command(&self.stream_id, &self.app)
    }

    /// Get the RTMP URL for this stream.
    pub fn rtmp_url(&self) -> String {
        format!("rtmp://{}:{}/{}", self.host, self.port, self.app)
    }
}

/// Create RTMP publish command in AMF0 format.
///
/// Returns the command bytes for a publish request.
pub fn create_rtmp_publish_command(stream_key: &str, app: &str) -> String {
    format!("publish {} live", stream_key)
}

/// Create RTMP play command in AMF0 format.
pub fn create_rtmp_play_command(stream_key: &str, app: &str) -> String {
    format!("play {}", stream_key)
}

/// Build RTMP connect command.
pub fn build_rtmp_connect_command(app: &str) -> Vec<u8> {
    let mut buf = BytesMut::with_capacity(256);

    // AMF0 Command: connect
    buf.extend_from_slice(b"\x02"); // String marker
    buf.extend_from_slice(b"connect");
    buf.extend_from_slice(b"\x00"); // Number marker
    buf.extend_from_slice(&1.0_f64.to_be_bytes()); // Transaction ID = 1
    buf.extend_from_slice(b"\x03"); // Object marker

    // Object properties
    buf.extend_from_slice(b"app");
    buf.extend_from_slice(b"\x02"); // String marker
    buf.extend_from_slice(app.as_bytes());
    buf.extend_from_slice(b"\x00\x00\x09"); // Object end

    buf.to_vec()
}

/// Build RTMP createStream command.
pub fn build_rtmp_create_stream_command() -> Vec<u8> {
    let mut buf = BytesMut::with_capacity(64);

    buf.extend_from_slice(b"\x02"); // String marker
    buf.extend_from_slice(b"createStream");
    buf.extend_from_slice(b"\x00"); // Number marker
    buf.extend_from_slice(&2.0_f64.to_be_bytes()); // Transaction ID = 2
    buf.extend_from_slice(b"\x05"); // Null marker

    buf.to_vec()
}

/// Build RTMP releaseStream command.
pub fn build_rtmp_release_stream_command(stream_key: &str) -> Vec<u8> {
    let mut buf = BytesMut::with_capacity(128);

    buf.extend_from_slice(b"\x02"); // String marker
    buf.extend_from_slice(b"releaseStream");
    buf.extend_from_slice(b"\x00"); // Number marker
    buf.extend_from_slice(&3.0_f64.to_be_bytes()); // Transaction ID = 3
    buf.extend_from_slice(b"\x05"); // Null marker
    buf.extend_from_slice(b"\x02"); // String marker
    buf.extend_from_slice(stream_key.as_bytes());

    buf.to_vec()
}

/// Build RTMP FCPublish command.
pub fn build_rtmp_fcpublish_command(stream_key: &str) -> Vec<u8> {
    let mut buf = BytesMut::with_capacity(128);

    buf.extend_from_slice(b"\x02"); // String marker
    buf.extend_from_slice(b"FCPublish");
    buf.extend_from_slice(b"\x00"); // Number marker
    buf.extend_from_slice(&4.0_f64.to_be_bytes()); // Transaction ID = 4
    buf.extend_from_slice(b"\x05"); // Null marker
    buf.extend_from_slice(b"\x02"); // String marker
    buf.extend_from_slice(stream_key.as_bytes());

    buf.to_vec()
}

/// Build RTMP publish command.
pub fn build_rtmp_publish_command(stream_key: &str, publish_type: &str) -> Vec<u8> {
    let mut buf = BytesMut::with_capacity(128);

    buf.extend_from_slice(b"\x02"); // String marker
    buf.extend_from_slice(b"publish");
    buf.extend_from_slice(b"\x00"); // Number marker
    buf.extend_from_slice(&0.0_f64.to_be_bytes()); // Transaction ID = 0
    buf.extend_from_slice(b"\x05"); // Null marker
    buf.extend_from_slice(b"\x02"); // String marker
    buf.extend_from_slice(stream_key.as_bytes());
    buf.extend_from_slice(b"\x02"); // String marker
    buf.extend_from_slice(publish_type.as_bytes());

    buf.to_vec()
}

/// RTMP chunk basic header size calculation.
pub fn chunk_header_size(chunk_stream_id: u8) -> usize {
    if chunk_stream_id < 2 {
        1
    } else if chunk_stream_id < 64 {
        2
    } else {
        3
    }
}

/// Build RTMP video message header for H264.
pub fn build_rtmp_h264_video_header(is_keyframe: bool, codec_id: u8) -> Vec<u8> {
    let mut header = Vec::with_capacity(5);

    // Frame type | Codec ID
    // Frame type: 1 = keyframe, 2 = inter
    let frame_type = if is_keyframe { 0x10 } else { 0x20 };
    header.push(frame_type | codec_id); // 0x17 = H264 keyframe, 0x27 = H264 inter

    // AVCPacketType = 0 (sequence header)
    header.push(0x00);

    // Composition time (3 bytes, zero for sequence header)
    header.extend_from_slice(&[0x00, 0x00, 0x00]);

    header
}

/// Parse RTMP chunk basic header.
pub fn parse_chunk_basic_header(data: &[u8]) -> Option<(u8, usize)> {
    if data.is_empty() {
        return None;
    }

    let byte = data[0];
    let fmt = (byte >> 6) & 0x03;
    let csid = byte & 0x3F;

    match fmt {
        0 => {
            // 2-byte header
            if data.len() < 2 {
                None
            } else {
                Some((csid, 2))
            }
        }
        1 => {
            // 3-byte header
            if data.len() < 3 {
                None
            } else {
                Some((csid, 3))
            }
        }
        _ => Some((csid, 1)), // 1-byte header
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtmp_test_helper() {
        let helper = RtmpTestHelper::new("test", "localhost", 1935);
        assert_eq!(helper.stream_id(), "test");
        assert_eq!(helper.port(), 1935);
    }

    #[test]
    fn test_build_connect_command() {
        let cmd = build_rtmp_connect_command("live");
        let cmd_str = String::from_utf8_lossy(&cmd);
        assert!(cmd_str.starts_with("\x02")); // String marker
        assert!(cmd_str.contains("connect"));
    }

    #[test]
    fn test_build_publish_command() {
        let cmd = build_rtmp_publish_command("stream", "live");
        let cmd_str = String::from_utf8_lossy(&cmd);
        assert!(cmd_str.contains("publish"));
        assert!(cmd_str.contains("stream"));
    }

    #[test]
    fn test_parse_chunk_basic_header() {
        // fmt = 3 (type 3), csid = 2 -> byte = 0xC2
        let result = parse_chunk_basic_header(&[0xC2]);
        assert!(result.is_some());
        let (csid, _size) = result.unwrap();
        assert_eq!(csid, 2);
    }
}
