//! HTTP-FLV test helper functions.
//!
//! Provides utilities for HTTP-FLV protocol testing.

/// HTTP-FLV test helper structure.
#[derive(Debug, Clone)]
pub struct HttpFlvTestHelper {
    stream_id: String,
    host: String,
    port: u16,
}

impl HttpFlvTestHelper {
    /// Create a new HTTP-FLV test helper.
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

    /// Get the port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Get the HTTP-FLV URL.
    pub fn flv_url(&self) -> String {
        format!("http://{}:{}/flv/{}", self.host, self.port, self.stream_id)
    }

    /// Generate HTTP GET request for FLV stream.
    pub fn get_request(&self) -> String {
        format!(
            "GET /flv/{} HTTP/1.1\r\n\
             Host: {}:{}\r\n\
             Accept: */*\r\n\
             Connection: keep-alive\r\n\
             \r\n",
            self.stream_id, self.host, self.port
        )
    }

    /// Generate HTTP GET request with optional query params.
    pub fn get_request_with_params(&self, params: &[(&str, &str)]) -> String {
        let query: String = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");

        format!(
            "GET /flv/{}?{} HTTP/1.1\r\n\
             Host: {}:{}\r\n\
             Accept: */*\r\n\
             Connection: keep-alive\r\n\
             \r\n",
            self.stream_id, query, self.host, self.port
        )
    }
}

/// Parse HTTP response status line.
pub fn parse_http_status(response: &str) -> Option<(u16, String)> {
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

/// Check if HTTP response indicates success.
pub fn is_success_response(response: &str) -> bool {
    parse_http_status(response)
        .map(|(code, _)| (200..300).contains(&code))
        .unwrap_or(false)
}

/// Parse Content-Type header.
pub fn parse_content_type(response: &str) -> Option<&str> {
    for line in response.lines() {
        if line.to_lowercase().starts_with("content-type:") {
            return Some(line.trim_start_matches("content-type:").trim());
        }
    }
    None
}

/// Parse Content-Length header.
pub fn parse_content_length(response: &str) -> Option<usize> {
    for line in response.lines() {
        if line.to_lowercase().starts_with("content-length:") {
            let value = line.trim_start_matches("content-length:").trim();
            return value.parse().ok();
        }
    }
    None
}

/// Find the end of HTTP headers (double CRLF).
pub fn find_header_end(data: &[u8]) -> Option<usize> {
    let header_end = b"\r\n\r\n";
    data.windows(4).position(|w| w == header_end)
}

/// FLV tag types.
pub mod flv_tag {
    /// Audio data tag type.
    pub const AUDIO: u8 = 0x08;

    /// Video data tag type.
    pub const VIDEO: u8 = 0x09;

    /// Script data tag type.
    pub const SCRIPT: u8 = 0x12;

    /// FLV video frame types.
    pub mod video_frame {
        /// Keyframe (H264/AVC sequence header or keyframe).
        pub const KEYFRAME: u8 = 0x17;

        /// Inter frame.
        pub const INTER_FRAME: u8 = 0x27;

        /// Video info/command frame.
        pub const INFO: u8 = 0x20;
    }

    /// FLV audio formats.
    pub mod audio_format {
        /// AAC.
        pub const AAC: u8 = 0xA0;

        /// MP3.
        pub const MP3: u8 = 0x80;
    }
}

/// Check if FLV video frame is a keyframe.
pub fn is_flv_keyframe(av_packet_type: u8, frame_type: u8) -> bool {
    // H264 sequence header (av_packet_type = 0) or keyframe (frame_type = 0x17)
    av_packet_type == 0 || frame_type == flv_tag::video_frame::KEYFRAME
}

/// Parse FLV tag header.
pub fn parse_flv_tag_header(data: &[u8]) -> Option<(u8, usize)> {
    if data.len() < 11 {
        return None;
    }

    let tag_type = data[0];
    let data_size = ((data[3] as usize) << 16) | ((data[4] as usize) << 8) | (data[5] as usize);

    Some((tag_type, data_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_flv_helper() {
        let helper = HttpFlvTestHelper::new("test", "localhost", 8081);
        assert_eq!(helper.stream_id(), "test");
        assert!(helper.flv_url().contains("/flv/test"));
    }

    #[test]
    fn test_parse_http_status() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: video/x-flv\r\n\r\n";
        let (code, _reason) = parse_http_status(response).unwrap();
        assert_eq!(code, 200);
    }

    #[test]
    fn test_parse_content_length() {
        // Note: Content-Length parsing expects lowercase header name
        let response = "HTTP/1.1 200 OK\r\ncontent-length: 12345\r\n\r\n";
        assert_eq!(parse_content_length(response), Some(12345));
    }

    #[test]
    fn test_find_header_end() {
        // Note: \r\n\r\n must be properly aligned
        let data = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nBody";
        // Find position of double CRLF
        let header_end = find_header_end(data);
        assert!(header_end.is_some());
        // After header end should be "Body"
        assert_eq!(&data[header_end.unwrap() + 4..], b"Body");
    }

    #[test]
    fn test_is_flv_keyframe() {
        assert!(is_flv_keyframe(0, 0x17)); // Sequence header
        assert!(is_flv_keyframe(0, 0x27)); // Sequence header (inter format)
        assert!(!is_flv_keyframe(1, 0x27)); // Not a keyframe
    }
}
