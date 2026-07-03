//! RTSP test helper functions.
//!
//! Provides utilities for RTSP protocol testing.

use super::TestCodec;
use std::time::{SystemTime, UNIX_EPOCH};

/// RTSP test helper structure.
#[derive(Debug, Clone)]
pub struct RtspTestHelper {
    stream_id: String,
    host: String,
    port: u16,
}

impl RtspTestHelper {
    /// Create a new RTSP test helper.
    pub fn new(stream_id: &str, host: &str, port: u16) -> Self {
        Self {
            stream_id: stream_id.to_string(),
            host: host.to_string(),
            port,
        }
    }

    /// Get the stream ID.
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Get the RTSP URL.
    pub fn rtsp_url(&self) -> String {
        format!("rtsp://{}:{}/{}", self.host, self.port, self.stream_id)
    }

    /// Generate OPTIONS request.
    pub fn options_request(&self) -> String {
        format!(
            "OPTIONS * RTSP/1.0\r\n\
             CSeq: {}\r\n\
             User-Agent: TestClient/1.0\r\n\
             \r\n",
            self.next_cseq()
        )
    }

    /// Generate DESCRIBE request.
    pub fn describe_request(&self) -> String {
        format!(
            "DESCRIBE {} RTSP/1.0\r\n\
             CSeq: {}\r\n\
             Accept: application/sdp\r\n\
             User-Agent: TestClient/1.0\r\n\
             \r\n",
            self.rtsp_url(),
            self.next_cseq()
        )
    }

    /// Generate SETUP request for video track.
    pub fn setup_video_request(&self, transport: &str) -> String {
        format!(
            "SETUP {}/trackID=0 RTSP/1.0\r\n\
             CSeq: {}\r\n\
             Transport: {}\r\n\
             User-Agent: TestClient/1.0\r\n\
             \r\n",
            self.rtsp_url(),
            self.next_cseq(),
            transport
        )
    }

    /// Generate SETUP request for audio track.
    pub fn setup_audio_request(&self, transport: &str) -> String {
        format!(
            "SETUP {}/trackID=1 RTSP/1.0\r\n\
             CSeq: {}\r\n\
             Transport: {}\r\n\
             User-Agent: TestClient/1.0\r\n\
             \r\n",
            self.rtsp_url(),
            self.next_cseq(),
            transport
        )
    }

    /// Generate PLAY request.
    pub fn play_request(&self, session: &str) -> String {
        format!(
            "PLAY {} RTSP/1.0\r\n\
             CSeq: {}\r\n\
             Session: {}\r\n\
             Range: npt=0.000-\r\n\
             User-Agent: TestClient/1.0\r\n\
             \r\n",
            self.rtsp_url(),
            self.next_cseq(),
            session
        )
    }

    /// Generate PAUSE request.
    pub fn pause_request(&self, session: &str) -> String {
        format!(
            "PAUSE {} RTSP/1.0\r\n\
             CSeq: {}\r\n\
             Session: {}\r\n\
             User-Agent: TestClient/1.0\r\n\
             \r\n",
            self.rtsp_url(),
            self.next_cseq(),
            session
        )
    }

    /// Generate TEARDOWN request.
    pub fn teardown_request(&self, session: &str) -> String {
        format!(
            "TEARDOWN {} RTSP/1.0\r\n\
             CSeq: {}\r\n\
             Session: {}\r\n\
             User-Agent: TestClient/1.0\r\n\
             \r\n",
            self.rtsp_url(),
            self.next_cseq(),
            session
        )
    }

    fn next_cseq(&self) -> u32 {
        static mut COUNTER: u32 = 0;
        unsafe {
            COUNTER += 1;
            COUNTER
        }
    }
}

/// Create RTSP ANNOUNCE SDP for publishing.
pub fn create_rtsp_announce_sdp(codec: TestCodec) -> String {
    match codec {
        TestCodec::H264 => create_h264_sdp(),
        TestCodec::H265 => create_h265_sdp(),
    }
}

/// Create SDP for H264 stream.
pub fn create_h264_sdp() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    format!(
        "v=0\r\n\
         o=- {} {} IN IP4 127.0.0.1\r\n\
         s=Test Stream\r\n\
         c=IN IP4 0.0.0.0\r\n\
         t=0 0\r\n\
         a=tool:libavformat\r\n\
         m=video 0 RTP/AVP 96\r\n\
         a=rtpmap:96 H264/90000\r\n\
         a=fmtp:96 packetization-mode=1;profile-level-id=42001E;sprop-parameter-sets=Z0IAKeKQCgC3ycQKAqCxI0DUFCgK1CFAQ==,aMuMsg==\r\n\
         m=audio 0 RTP/AVP 97\r\n\
         a=rtpmap:97 mpeg4-generic/44100/2\r\n\
         a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3\r\n"
        , ts, ts
    )
}

/// Create SDP for H265 stream.
pub fn create_h265_sdp() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    format!(
        "v=0\r\n\
         o=- {} {} IN IP4 127.0.0.1\r\n\
         s=Test H265 Stream\r\n\
         c=IN IP4 0.0.0.0\r\n\
         t=0 0\r\n\
         a=tool:libavformat\r\n\
         m=video 0 RTP/AVP 98\r\n\
         a=rtpmap:98 H265/90000\r\n\
         a=fmtp:98 profile-id=1;tier-flag=0;level-id=93;sprop-layer-id=1;sprop-layer-info=NAL...;sprop-vps=NAL...;sprop-sps=NAL...;sprop-pps=NAL...\r\n\
         m=audio 0 RTP/AVP 97\r\n\
         a=rtpmap:97 mpeg4-generic/44100/2\r\n\
         a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3\r\n"
        , ts, ts
    )
}

/// Parse RTSP response status line.
pub fn parse_rtsp_status(response: &str) -> Option<(u16, String)> {
    let first_line = response.lines().next()?;
    let parts: Vec<&str> = first_line.split_whitespace().collect();

    if parts.len() < 2 {
        return None;
    }

    let status_code: u16 = parts[1].parse().ok()?;
    let reason = if parts.len() >= 3 {
        parts[2..].join(" ")
    } else {
        String::new()
    };

    Some((status_code, reason))
}

/// Parse RTSP Session header.
pub fn parse_session_header(response: &str) -> Option<String> {
    for line in response.lines() {
        if line.to_lowercase().starts_with("session:") {
            let value = line.trim_start_matches("session:");
            return Some(value.trim().split(';').next().unwrap_or(value.trim()).to_string());
        }
    }
    None
}

/// Parse RTSP CSeq header.
pub fn parse_cseq(response: &str) -> Option<u32> {
    for line in response.lines() {
        if line.to_lowercase().starts_with("cseq:") {
            let value = line.trim_start_matches("cseq:");
            return value.trim().parse().ok();
        }
    }
    None
}

/// RTSP transport options.
pub mod transport {
    /// RTP/AVP over TCP (interleaved).
    pub const RTP_AVP_TCP: &str = "RTP/AVP/TCP;interleaved=0-1";

    /// RTP/AVP over UDP.
    pub const RTP_AVP_UDP: &str = "RTP/AVP;unicast;client_port=5000-5001";

    /// RTP/AVP/TCP for video.
    pub const VIDEO_TCP: &str = "RTP/AVP/TCP;unicast;interleaved=0-1";

    /// RTP/AVP/TCP for audio.
    pub const AUDIO_TCP: &str = "RTP/AVP/TCP;unicast;interleaved=2-3";

    /// RTP/AVP/UDP for video.
    pub const VIDEO_UDP: &str = "RTP/AVP;unicast;client_port=5000-5001;server_port=6000-6001";

    /// RTP/AVP/UDP for audio.
    pub const AUDIO_UDP: &str = "RTP/AVP;unicast;client_port=5002-5003;server_port=6002-6003";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtsp_test_helper() {
        let helper = RtspTestHelper::new("test", "localhost", 554);
        assert_eq!(helper.stream_id(), "test");
        assert!(helper.rtsp_url().contains("rtsp://"));
    }

    #[test]
    fn test_parse_rtsp_status() {
        let response = "RTSP/1.0 200 OK\r\nCSeq: 1\r\n\r\n";
        let (code, reason) = parse_rtsp_status(response).unwrap();
        assert_eq!(code, 200);
        assert_eq!(reason, "OK");
    }

    #[test]
    fn test_parse_session() {
        let response = "RTSP/1.0 200 OK\r\nSession: abc123;timeout=60\r\n\r\n";
        let session = parse_session_header(response);
        assert!(session.is_some());
        // Session should contain abc123
        assert!(session.unwrap().contains("abc123"));
    }

    #[test]
    fn test_create_h264_sdp() {
        let sdp = create_h264_sdp();
        assert!(sdp.contains("H264/90000"));
        assert!(sdp.contains("v=0"));
    }

    #[test]
    fn test_create_h265_sdp() {
        let sdp = create_h265_sdp();
        assert!(sdp.contains("H265/90000"));
        assert!(sdp.contains("v=0"));
    }
}
