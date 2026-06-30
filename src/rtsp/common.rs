use anyhow::Result;
use bytes::BytesMut;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tracing::{info, warn, debug};

use crate::core::{Track, CodecType};
use super::messages::RtspRequest;

/// Format RTSP message for human-readable logging (CRLF → LF, trim trailing blank line).
pub fn format_rtsp_message(message: &str) -> String {
    let mut lines: Vec<String> = message
        .split("\r\n")
        .map(|line| {
            if line.is_empty() {
                "[empty line]".to_string()
            } else {
                line.to_string()
            }
        })
        .collect();

    if lines.last() == Some(&"[empty line]".to_string()) {
        lines.pop();
    }

    lines.join("\n")
}

/// Parsed `Transport:` header from an RTSP message.
#[derive(Debug, Default, Clone)]
pub struct TransportInfo {
    pub transport_type: String,
    pub client_port: Option<(u16, u16)>,
    pub server_port: Option<(u16, u16)>,
}

pub fn extract_transport(request: &str) -> TransportInfo {
    let mut info = TransportInfo::default();

    for line in request.lines() {
        if !line.starts_with("Transport:") {
            continue;
        }
        for part in line.split(';') {
            let trimmed = part.trim();
            if trimmed.starts_with("Transport:") {
                info.transport_type = trimmed["Transport:".len()..].trim().to_string();
            } else if trimmed.starts_with("client_port=") {
                let ports: Vec<&str> = trimmed["client_port=".len()..].split('-').collect();
                if ports.len() >= 2 {
                    if let (Ok(p1), Ok(p2)) = (ports[0].parse(), ports[1].parse()) {
                        info.client_port = Some((p1, p2));
                    }
                }
            } else if trimmed.starts_with("server_port=") {
                let ports: Vec<&str> = trimmed["server_port=".len()..].split('-').collect();
                if ports.len() >= 2 {
                    if let (Ok(p1), Ok(p2)) = (ports[0].parse(), ports[1].parse()) {
                        info.server_port = Some((p1, p2));
                    }
                }
            } else if trimmed.starts_with("interleaved=") {
                let ports: Vec<&str> = trimmed["interleaved=".len()..].split('-').collect();
                if ports.len() >= 2 {
                    if let (Ok(p1), Ok(p2)) = (ports[0].parse(), ports[1].parse()) {
                        info.client_port = Some((p1, p2));
                    }
                }
            }
        }
        break;
    }

    info
}

pub fn is_udp_transport(transport: &TransportInfo) -> bool {
    !transport.transport_type.to_uppercase().contains("TCP")
}

/// Parse `server_port=` from a SETUP response.
pub fn parse_transport_server_ports(response: &str) -> Option<(u16, u16)> {
    for line in response.lines() {
        if !line.starts_with("Transport:") {
            continue;
        }
        for part in line.split(';') {
            let trimmed = part.trim();
            if let Some(ports) = trimmed.strip_prefix("server_port=") {
                let v: Vec<&str> = ports.split('-').collect();
                if v.len() >= 2 {
                    if let (Ok(p1), Ok(p2)) = (v[0].parse(), v[1].parse()) {
                        return Some((p1, p2));
                    }
                }
            }
        }
    }
    None
}

/// True when URL query asks for UDP (`transport=udp` or `rtsp_transport=udp`).
pub fn url_prefers_udp(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.contains("transport=udp") || lower.contains("rtsp_transport=udp")
}

/// Parse track index from SETUP URL (`trackID=`, `trackid=`, `streamid=`).
pub fn extract_track_id(url: &str) -> u32 {
    let path = url.to_lowercase();
    for prefix in ["trackid=", "streamid="] {
        if let Some(idx) = path.find(prefix) {
            let rest = &path[idx + prefix.len()..];
            if let Some(end) = rest.find(|c: char| !c.is_ascii_digit()) {
                return rest[..end].parse().unwrap_or(0);
            }
            return rest.parse().unwrap_or(0);
        }
    }
    0
}

/// Parse stream name from RTSP URL, stripping track suffixes.
pub fn extract_stream_id(url: &str) -> String {
    let path = url::Url::parse(url)
        .ok()
        .map(|u| u.path().to_string())
        .unwrap_or_else(|| url.to_string());

    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if let Some(last_part) = parts.last() {
        let lower = last_part.to_lowercase();
        if lower.starts_with("trackid=") || lower.starts_with("streamid=") {
            if parts.len() >= 2 {
                return parts[parts.len() - 2].to_string();
            }
        }
        last_part.to_string()
    } else {
        "live".to_string()
    }
}

pub struct RtspCommon;

impl RtspCommon {
    pub async fn read_response(reader: &mut tokio::net::tcp::OwnedReadHalf) -> Result<String> {
        let mut buffer = BytesMut::with_capacity(4096);
        let mut response = String::new();
        let mut content_length: Option<usize> = None;
        let mut headers_complete = false;

        loop {
            let bytes_read = reader.read_buf(&mut buffer).await?;
            if bytes_read == 0 {
                break;
            }

            let current = String::from_utf8_lossy(&buffer);
            response.push_str(&current);

            if !headers_complete {
                if let Some(pos) = response.find("\r\n\r\n") {
                    headers_complete = true;
                    let headers = &response[..pos];
                    for line in headers.split("\r\n") {
                        if line.starts_with("Content-Length:") {
                            if let Some(value) = line.split(":").nth(1) {
                                content_length = value.trim().parse().ok();
                            }
                        }
                    }
                }
            }

            if headers_complete {
                let body_start = response.find("\r\n\r\n").map(|p| p + 4).unwrap_or(0);
                let body_length = response.len() - body_start;

                if let Some(cl) = content_length {
                    if body_length >= cl {
                        break;
                    }
                } else if response.ends_with("\r\n\r\n") {
                    break;
                }
            }

            buffer.clear();
        }

        Ok(response)
    }

    pub fn parse_interleaved(data: &[u8]) -> Option<(u8, &[u8])> {
        if data.is_empty() || data[0] != 0x24 {
            return None;
        }

        if data.len() < 4 {
            return None;
        }

        let channel = data[1];
        let length = ((data[2] as usize) << 8) | (data[3] as usize);

        if data.len() < 4 + length {
            return None;
        }

        Some((channel, &data[4..4 + length]))
    }

    pub fn build_rtp_packet(payload_type: u8, seq: u16, ts: u32, ssrc: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(12 + payload.len());
        buf.push((2 << 6) | 0);
        buf.push(((marker as u8) << 7) | payload_type);
        buf.extend_from_slice(&seq.to_be_bytes());
        buf.extend_from_slice(&ts.to_be_bytes());
        buf.extend_from_slice(&ssrc.to_be_bytes());
        buf.extend_from_slice(payload);
        buf
    }

    // Convert frame timestamp to RTP timestamp based on clock rate
    // frame_timestamp: timestamp in milliseconds
    // clock_rate: RTP clock rate (e.g., 90000 for H264 video, 44100 for AAC audio)
    pub fn convert_timestamp_to_rtp(frame_timestamp_ms: u64, clock_rate: u32) -> u32 {
        ((frame_timestamp_ms * clock_rate as u64) / 1000) as u32
    }

    // Convert RTP timestamp to milliseconds
    pub fn convert_rtp_timestamp_to_ms(rtp_timestamp: u32, clock_rate: u32) -> u64 {
        ((rtp_timestamp as u64) * 1000) / clock_rate as u64
    }

    pub fn wrap_interleaved(data: &[u8], channel: u8) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + data.len());
        buf.push(0x24);
        buf.push(channel);
        buf.extend_from_slice(&((data.len() as u16).to_be_bytes()));
        buf.extend_from_slice(data);
        buf
    }

    pub fn parse_sdp_tracks(sdp: &str) -> Vec<Track> {
        let mut tracks = Vec::new();
        let lines: Vec<&str> = sdp.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];
            i += 1;

            if line.starts_with("m=") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    let payload_type: u8 = parts[3].parse().unwrap_or(96);

                    let mut codec = CodecType::H264;
                    let mut clock_rate = 90000;

                    while i < lines.len() {
                        let next_line = lines[i];

                        if next_line.starts_with("a=rtpmap:") {
                            let rtpmap_parts: Vec<&str> = next_line.split_whitespace().collect();
                            if rtpmap_parts.len() >= 2 {
                                let codec_info = rtpmap_parts[1];
                                let codec_parts: Vec<&str> = codec_info.split('/').collect();
                                if codec_parts.len() >= 2 {
                                    codec = match codec_parts[0].to_lowercase().as_str() {
                                        "h264" => CodecType::H264,
                                        "h265" => CodecType::H265,
                                        "mpeg4-generic" | "aac" => CodecType::AAC,
                                        "opus" => CodecType::Opus,
                                        "pcmu" => CodecType::G711,
                                        _ => CodecType::H264,
                                    };
                                    clock_rate = codec_parts[1].parse().unwrap_or(90000);
                                }
                            }
                            i += 1;
                        } else if next_line.starts_with("m=") || next_line.starts_with("s=") {
                            break;
                        } else {
                            i += 1;
                        }
                    }

                    let track_id = tracks.len() as u8;
                    tracks.push(Track {
                        id: track_id,
                        codec,
                        payload_type,
                        clock_rate,
                        extra_params: std::collections::HashMap::new(),
                    });
                }
            }
        }

        tracks
    }

    // Parse SDP and extract SPS/PPS from fmtp line
    pub fn parse_sdp_with_sps_pps(sdp: &str) -> (Vec<Track>, Option<Vec<u8>>, Option<Vec<u8>>) {
        let mut tracks = Vec::new();
        let mut sps: Option<Vec<u8>> = None;
        let mut pps: Option<Vec<u8>> = None;
        let lines: Vec<&str> = sdp.lines().collect();
        let mut i = 0;

        debug!("[RTSP Common] parse_sdp_with_sps_pps called, {} lines total", lines.len());

        while i < lines.len() {
            let line = lines[i];
            debug!("[RTSP Common] Line {}: '{}'", i, line);

            // Extract SPS/PPS from a=fmtp line
            if line.starts_with("a=fmtp:") {
                debug!("[RTSP Common] Found fmtp line: {}", line);
                let fmtp_parts: Vec<&str> = line.split_whitespace().collect();
                debug!("[RTSP Common] fmtp_parts count: {}", fmtp_parts.len());
                for (idx, part) in fmtp_parts.iter().enumerate() {
                    debug!("[RTSP Common] fmtp_part[{}] = '{}'", idx, part);
                    if part.starts_with("sprop-parameter-sets=") {
                        let params = part.strip_prefix("sprop-parameter-sets=").unwrap_or("");
                        debug!("[RTSP Common] params after strip: '{}'", params);
                        // Remove trailing semicolon if present
                        let params = params.trim_end_matches(';');
                        debug!("[RTSP Common] params after trim: '{}'", params);
                        // SPS and PPS are comma-separated
                        let param_parts: Vec<&str> = params.split(',').collect();
                        debug!("[RTSP Common] Found sprop-parameter-sets: {} parts", param_parts.len());
                        
                        for (idx, param) in param_parts.iter().enumerate() {
                            debug!("[RTSP Common] param[{}] = '{}', is_empty={}", idx, param, param.is_empty());
                            if !param.is_empty() {
                                use base64::Engine;
                                match base64::engine::general_purpose::STANDARD.decode(param) {
                                    Ok(decoded) => {
                                        if decoded.len() >= 2 {
                                            let nal_type = decoded[0] & 0x1F;
                                            debug!("[RTSP Common] Decoded parameter set {}: length={} bytes, nal_type={}", 
                                                   idx, decoded.len(), nal_type);
                                            if nal_type == 7 && sps.is_none() {
                                                info!("[RTSP Common] Found SPS in SDP: {} bytes", decoded.len());
                                                debug!("[RTSP Common] SPS first 16 bytes: {:02X?}", &decoded[..std::cmp::min(16, decoded.len())]);
                                                sps = Some(decoded);
                                            } else if nal_type == 8 && pps.is_none() {
                                                info!("[RTSP Common] Found PPS in SDP: {} bytes", decoded.len());
                                                debug!("[RTSP Common] PPS first 16 bytes: {:02X?}", &decoded[..std::cmp::min(16, decoded.len())]);
                                                pps = Some(decoded);
                                            }
                                        } else {
                                            warn!("[RTSP Common] Decoded parameter set too short: {} bytes", decoded.len());
                                        }
                                    },
                                    Err(e) => {
                                        warn!("[RTSP Common] Failed to decode base64 parameter: {}, error: {}", param, e);
                                    }
                                }
                            }
                        }
                    }
                }
                i += 1;
                continue;
            }

            if line.starts_with("m=") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    let payload_type: u8 = parts[3].parse().unwrap_or(96);
                    let mut codec = CodecType::H264;
                    let mut clock_rate = 90000;

                    i += 1;
                    while i < lines.len() {
                        let next_line = lines[i];

                        if next_line.starts_with("a=rtpmap:") {
                            let rtpmap_parts: Vec<&str> = next_line.split_whitespace().collect();
                            if rtpmap_parts.len() >= 2 {
                                let codec_info = rtpmap_parts[1];
                                let codec_parts: Vec<&str> = codec_info.split('/').collect();
                                if codec_parts.len() >= 2 {
                                    codec = match codec_parts[0].to_lowercase().as_str() {
                                        "h264" => CodecType::H264,
                                        "h265" => CodecType::H265,
                                        "mpeg4-generic" | "aac" => CodecType::AAC,
                                        "opus" => CodecType::Opus,
                                        "pcmu" => CodecType::G711,
                                        _ => CodecType::H264,
                                    };
                                    clock_rate = codec_parts[1].parse().unwrap_or(90000);
                                }
                            }
                            i += 1;
                        } else if next_line.starts_with("a=fmtp:") && sps.is_none() {
                            // Extract SPS/PPS from fmtp line for H264
                            debug!("[RTSP Common] Found fmtp line inside m= loop: {}", next_line);
                            let fmtp_parts: Vec<&str> = next_line.split_whitespace().collect();
                            for part in fmtp_parts {
                                if part.starts_with("sprop-parameter-sets=") {
                                    let params = part.strip_prefix("sprop-parameter-sets=").unwrap_or("");
                                    let params = params.trim_end_matches(';');
                                    let param_parts: Vec<&str> = params.split(',').collect();
                                    debug!("[RTSP Common] Found sprop-parameter-sets inside m= loop: {} parts", param_parts.len());
                                    
                                    for (idx, param) in param_parts.iter().enumerate() {
                                        debug!("[RTSP Common] param[{}] = '{}'", idx, param);
                                        if !param.is_empty() {
                                            use base64::Engine;
                                            match base64::engine::general_purpose::STANDARD.decode(param) {
                                                Ok(decoded) => {
                                                    if decoded.len() >= 2 {
                                                        let nal_type = decoded[0] & 0x1F;
                                                        debug!("[RTSP Common] Decoded parameter set {}: length={} bytes, nal_type={}", 
                                                               idx, decoded.len(), nal_type);
                                                        if nal_type == 7 && sps.is_none() {
                                                            info!("[RTSP Common] Found SPS in SDP: {} bytes", decoded.len());
                                                            sps = Some(decoded);
                                                        } else if nal_type == 8 && pps.is_none() {
                                                            info!("[RTSP Common] Found PPS in SDP: {} bytes", decoded.len());
                                                            pps = Some(decoded);
                                                        }
                                                    }
                                                },
                                                Err(e) => {
                                                    warn!("[RTSP Common] Failed to decode base64 parameter: {}, error: {}", param, e);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            i += 1;
                        } else if next_line.starts_with("m=") || next_line.starts_with("s=") {
                            break;
                        } else {
                            i += 1;
                        }
                    }

                    tracks.push(Track {
                        id: (tracks.len()) as u8,
                        codec,
                        payload_type,
                        clock_rate,
                        extra_params: std::collections::HashMap::new(),
                    });
                    continue;
                }
            }
            i += 1;
        }

        (tracks, sps, pps)
    }

    pub fn build_sdp(tracks: &[Track]) -> String {
        let mut sdp = String::new();
        sdp.push_str("v=0\r\n");
        sdp.push_str("o=- 0 0 IN IP4 127.0.0.1\r\n");
        sdp.push_str("s=MediaServer Session\r\n");
        sdp.push_str("t=0 0\r\n");

        for (idx, track) in tracks.iter().enumerate() {
            let (media_type, codec_name) = match track.codec {
                CodecType::H264 => ("video", "H264"),
                CodecType::H265 => ("video", "H265"),
                CodecType::AAC => ("audio", "mpeg4-generic"),
                CodecType::Opus => ("audio", "opus"),
                CodecType::G711 => ("audio", "PCMU"),
                _ => ("video", "H264"),
            };

            sdp.push_str(&format!("m={} 0 RTP/AVP {}\r\n", media_type, track.payload_type));
            sdp.push_str("c=IN IP4 0.0.0.0\r\n");
            sdp.push_str("t=0 0\r\n");
            sdp.push_str(&format!("a=rtpmap:{} {}/{}\r\n", track.payload_type, codec_name, track.clock_rate));
            sdp.push_str(&format!("a=control:trackID={}\r\n", idx));

            if track.codec == CodecType::H264 {
                sdp.push_str("a=fmtp:96 packetization-mode=1;profile-level-id=42E01F;sprop-parameter-sets=Z0LAHukBQBbsAAADAAQAAAMABAAAAwHNgYI=\r\n");
            } else if track.codec == CodecType::AAC {
                sdp.push_str("a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;\r\n");
            }
        }

        sdp
    }

    pub fn extract_session_id(response: &str) -> Option<String> {
        for line in response.lines() {
            if line.starts_with("Session:") {
                let parts: Vec<&str> = line.split(';').collect();
                if let Some(session_part) = parts.first() {
                    if let Some(session_id) = session_part.split(':').nth(1) {
                        return Some(session_id.trim().to_string());
                    }
                }
            }
        }
        None
    }

    pub async fn write_request(writer: &mut tokio::net::tcp::OwnedWriteHalf, request: &RtspRequest) -> Result<()> {
        let request_str = request.to_string();
        writer.write_all(request_str.as_bytes()).await?;
        writer.flush().await?;
        Ok(())
    }

    pub async fn write_response(writer: &mut tokio::net::tcp::OwnedWriteHalf, response: &str) -> Result<()> {
        writer.write_all(response.as_bytes()).await?;
        writer.flush().await?;
        Ok(())
    }

    pub fn parse_rtp_header(data: &[u8]) -> Option<RtpHeader> {
        if data.len() < 12 {
            return None;
        }

        let version = (data[0] >> 6) & 0x03;
        let padding = (data[0] >> 5) & 0x01;
        let extension = (data[0] >> 4) & 0x01;
        let csrc_count = data[0] & 0x0F;
        let marker = (data[1] >> 7) & 0x01;
        let payload_type = data[1] & 0x7F;
        let sequence_number = ((data[2] as u16) << 8) | (data[3] as u16);
        let timestamp = ((data[4] as u32) << 24) | ((data[5] as u32) << 16) 
                      | ((data[6] as u32) << 8) | (data[7] as u32);
        let ssrc = ((data[8] as u32) << 24) | ((data[9] as u32) << 16) 
                 | ((data[10] as u32) << 8) | (data[11] as u32);

        Some(RtpHeader {
            version,
            padding,
            extension,
            csrc_count,
            marker,
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
        })
    }

    pub async fn create_udp_socket(local_port: u16) -> Result<UdpSocket> {
        let addr = format!("0.0.0.0:{}", local_port);
        let socket = UdpSocket::bind(&addr).await?;
        info!("[RTSP Common] UDP socket bound to {}", addr);
        Ok(socket)
    }

    pub async fn send_rtp_over_udp(socket: &UdpSocket, data: &[u8], dest: SocketAddr) -> Result<usize> {
        let sent = socket.send_to(data, dest).await?;
        debug!("[RTSP Common] Sent {} bytes RTP over UDP to {}", sent, dest);
        Ok(sent)
    }

    pub async fn receive_rtp_over_udp(socket: &UdpSocket, buffer: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let (len, src) = socket.recv_from(buffer).await?;
        debug!("[RTSP Common] Received {} bytes RTP over UDP from {}", len, src);
        Ok((len, src))
    }

    pub fn build_rtcp_rr(ssrc: u32, last_seq: u16, jitter: u32, lsr: u32, dlsr: u32) -> Vec<u8> {
        let mut rtcp = Vec::with_capacity(32);
        let report_count = 0;
        
        rtcp.push((2 << 6) | (report_count & 0x1F));
        rtcp.push(201);
        rtcp.extend_from_slice(&((4u16).to_be_bytes()));
        rtcp.extend_from_slice(&ssrc.to_be_bytes());
        rtcp.extend_from_slice(&ssrc.to_be_bytes());
        rtcp.extend_from_slice(&last_seq.to_be_bytes());
        rtcp.extend_from_slice(&jitter.to_be_bytes());
        rtcp.extend_from_slice(&lsr.to_be_bytes());
        rtcp.extend_from_slice(&dlsr.to_be_bytes());
        
        rtcp
    }

    pub fn build_rtcp_sr(ssrc: u32, timestamp: u32, packet_count: u32, octet_count: u32) -> Vec<u8> {
        let mut rtcp = Vec::with_capacity(28);
        let report_count = 0;
        
        rtcp.push((2 << 6) | (report_count & 0x1F));
        rtcp.push(200);
        rtcp.extend_from_slice(&((6u16).to_be_bytes()));
        rtcp.extend_from_slice(&ssrc.to_be_bytes());
        
        let ntp_sec = (timestamp as u64 >> 32) as u32;
        let ntp_frac = timestamp;
        rtcp.extend_from_slice(&ntp_sec.to_be_bytes());
        rtcp.extend_from_slice(&ntp_frac.to_be_bytes());
        rtcp.extend_from_slice(&timestamp.to_be_bytes());
        rtcp.extend_from_slice(&packet_count.to_be_bytes());
        rtcp.extend_from_slice(&octet_count.to_be_bytes());
        
        rtcp
    }

    pub fn is_rtcp_packet(data: &[u8]) -> bool {
        if data.len() < 2 {
            return false;
        }
        let payload_type = data[1];
        payload_type == 200 || payload_type == 201 || payload_type == 202 || payload_type == 203
    }

    // H264 NAL unit types
    pub const H264_NAL_SPS: u8 = 7;
    pub const H264_NAL_PPS: u8 = 8;
    pub const H264_NAL_IDR: u8 = 5;
    pub const H264_NAL_NON_IDR: u8 = 1;
    pub const H264_NAL_SEI: u8 = 6;

    // Parse H264 NAL units from RTP payload
    // Returns (start_codes_removed, contains_sps, contains_pps, contains_idr)
    pub fn parse_h264_nal_units(payload: &[u8]) -> (Vec<Vec<u8>>, bool, bool, bool) {
        let mut nal_units: Vec<Vec<u8>> = Vec::new();
        let mut contains_sps = false;
        let mut contains_pps = false;
        let mut contains_idr = false;

        // Check if payload uses start codes (00 00 00 01 or 00 00 01)
        if payload.len() >= 4 && payload[0] == 0x00 && payload[1] == 0x00 {
            let start_code_len = if payload[2] == 0x01 { 3 } else { 4 };
            let mut i = start_code_len;

            while i < payload.len() {
                // Find next start code
                let mut next_start = i;
                while next_start < payload.len() - 4 {
                    if payload[next_start] == 0x00 && payload[next_start + 1] == 0x00 {
                        if payload[next_start + 2] == 0x01 {
                            break;
                        } else if payload[next_start + 2] == 0x00 && payload[next_start + 3] == 0x01 {
                            next_start += 1;
                            break;
                        }
                    }
                    next_start += 1;
                }

                let nal_type = if payload[i] & 0x1F == 0 {
                    payload[i + 1] & 0x1F
                } else {
                    payload[i] & 0x1F
                };

                let nal_unit = if next_start < payload.len() - 4 {
                    payload[i..next_start].to_vec()
                } else {
                    payload[i..].to_vec()
                };

                if !nal_unit.is_empty() {
                    match nal_type {
                        7 => contains_sps = true,
                        8 => contains_pps = true,
                        5 => contains_idr = true,
                        _ => {}
                    }
                    nal_units.push(nal_unit);
                }

                i = next_start + (if payload[next_start + 2] == 0x01 { 3 } else { 4 });
                if i >= payload.len() {
                    break;
                }
            }
        } else if !payload.is_empty() {
            // Single NAL unit without start codes (should already have length prefix removed)
            nal_units.push(payload.to_vec());
            let nal_type = payload[0] & 0x1F;
            match nal_type {
                7 => contains_sps = true,
                8 => contains_pps = true,
                5 => contains_idr = true,
                _ => {}
            }
        }

        (nal_units, contains_sps, contains_pps, contains_idr)
    }

    // Extract SPS and PPS from H264 data
    pub fn extract_sps_pps(payload: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
        let mut sps: Option<Vec<u8>> = None;
        let mut pps: Option<Vec<u8>> = None;

        debug!("[RTSP Common] extract_sps_pps: payload length={} bytes", payload.len());

        // Check if payload uses start codes (00 00 00 01 or 00 00 01)
        if payload.len() >= 4 && payload[0] == 0x00 && payload[1] == 0x00 {
            let start_code_len = if payload[2] == 0x01 { 3 } else { 4 };
            debug!("[RTSP Common] Found start code, length={}", start_code_len);
            
            let mut i = start_code_len;

            while i < payload.len() {
                // Find next start code
                let mut next_start = i;
                while next_start < payload.len() - 4 {
                    if payload[next_start] == 0x00 && payload[next_start + 1] == 0x00 {
                        if payload[next_start + 2] == 0x01 {
                            break;
                        } else if payload[next_start + 2] == 0x00 && payload[next_start + 3] == 0x01 {
                            next_start += 1;
                            break;
                        }
                    }
                    next_start += 1;
                }

                let nal_unit = if next_start < payload.len() - 4 {
                    payload[i..next_start].to_vec()
                } else {
                    payload[i..].to_vec()
                };

                if !nal_unit.is_empty() {
                    let nal_type = nal_unit[0] & 0x1F;
                    debug!("[RTSP Common] Found NAL unit: type={}, length={} bytes", nal_type, nal_unit.len());
                    
                    match nal_type {
                        7 if sps.is_none() => {
                            info!("[RTSP Common] Found SPS in start-code formatted data: {} bytes", nal_unit.len());
                            debug!("[RTSP Common] SPS first 16 bytes: {:02X?}", &nal_unit[..std::cmp::min(16, nal_unit.len())]);
                            sps = Some(nal_unit);
                        },
                        8 if pps.is_none() => {
                            info!("[RTSP Common] Found PPS in start-code formatted data: {} bytes", nal_unit.len());
                            debug!("[RTSP Common] PPS first 16 bytes: {:02X?}", &nal_unit[..std::cmp::min(16, nal_unit.len())]);
                            pps = Some(nal_unit);
                        },
                        _ => {
                            debug!("[RTSP Common] Skipping non-SPS/PPS NAL type {}", nal_type);
                        }
                    }
                }

                i = next_start + (if payload[next_start + 2] == 0x01 { 3 } else { 4 });
                if i >= payload.len() {
                    break;
                }
            }
        } else if !payload.is_empty() {
            // Single NAL unit without start codes
            let nal_type = payload[0] & 0x1F;
            debug!("[RTSP Common] Single NAL unit without start code: type={}, length={} bytes", 
                   nal_type, payload.len());
            
            match nal_type {
                7 => {
                    info!("[RTSP Common] Found SPS in single NAL unit: {} bytes", payload.len());
                    debug!("[RTSP Common] SPS first 16 bytes: {:02X?}", &payload[..std::cmp::min(16, payload.len())]);
                    sps = Some(payload.to_vec());
                },
                8 => {
                    info!("[RTSP Common] Found PPS in single NAL unit: {} bytes", payload.len());
                    debug!("[RTSP Common] PPS first 16 bytes: {:02X?}", &payload[..std::cmp::min(16, payload.len())]);
                    pps = Some(payload.to_vec());
                },
                _ => {
                    debug!("[RTSP Common] Single NAL unit is not SPS/PPS (type={})", nal_type);
                }
            }
        } else {
            debug!("[RTSP Common] Payload is empty, returning None for both SPS and PPS");
        }

        debug!("[RTSP Common] extract_sps_pps completed: SPS={}, PPS={}", sps.is_some(), pps.is_some());
        (sps, pps)
    }

    // Convert H264 NAL unit to RTP format (add NAL header if needed)
    pub fn h264_nal_to_rtp_payload(nal_unit: &[u8]) -> Vec<u8> {
        // Add length prefix for proper RTP payload
        let mut result = Vec::with_capacity(4 + nal_unit.len());
        result.extend_from_slice(&(nal_unit.len() as u32).to_be_bytes());
        result.extend_from_slice(nal_unit);
        result
    }

    // Create RTP packet with SPS/PPS for H264 keyframe
    pub fn create_h264_keyframe_rtp_packets(
        sps: &[u8],
        pps: &[u8],
        seq: u16,
        timestamp: u32,
        ssrc: u32
    ) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();

        // Send SPS packet
        if !sps.is_empty() {
            let mut rtp = Self::build_rtp_packet(96, seq, timestamp, ssrc, false, sps);
            packets.push(rtp);
        }

        // Send PPS packet
        if !pps.is_empty() {
            let mut rtp = Self::build_rtp_packet(96, seq + 1, timestamp, ssrc, false, pps);
            packets.push(rtp);
        }

        packets
    }

    // Convert NAL units to RTP packet payload format (length-prefixed)
    pub fn h264_nal_units_to_rtp_payload(nal_units: &[Vec<u8>]) -> Vec<u8> {
        let mut payload = Vec::new();
        for nal in nal_units {
            // Add length prefix (4 bytes, big-endian)
            payload.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            payload.extend_from_slice(nal);
        }
        payload
    }

    /// Max RTP payload size for UDP (stay under typical Ethernet MTU).
    pub const UDP_RTP_MAX_PAYLOAD: usize = 1200;

    /// Packetize one H.264 NAL into one or more RTP packets (single-NAL or FU-A).
    pub fn packetize_h264_nal_for_rtp(
        nal: &[u8],
        payload_type: u8,
        seq: &mut u16,
        ts: u32,
        ssrc: u32,
        marker: bool,
    ) -> Vec<Vec<u8>> {
        if nal.is_empty() {
            return Vec::new();
        }

        if nal.len() <= Self::UDP_RTP_MAX_PAYLOAD {
            let pkt = Self::build_rtp_packet(payload_type, *seq, ts, ssrc, marker, nal);
            *seq = seq.wrapping_add(1);
            return vec![pkt];
        }

        let nal_type = nal[0] & 0x1F;
        let fu_indicator = (nal[0] & 0x60) | 28;
        let data = &nal[1..];
        let chunk_max = Self::UDP_RTP_MAX_PAYLOAD - 2;
        let mut packets = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            let chunk_size = (data.len() - offset).min(chunk_max);
            let is_start = offset == 0;
            let is_end = offset + chunk_size >= data.len();
            let fu_header =
                (if is_start { 0x80 } else { 0 }) | (if is_end { 0x40 } else { 0 }) | nal_type;
            let mut payload = vec![fu_indicator, fu_header];
            payload.extend_from_slice(&data[offset..offset + chunk_size]);
            let mark = marker && is_end;
            packets.push(Self::build_rtp_packet(
                payload_type, *seq, ts, ssrc, mark, &payload,
            ));
            *seq = seq.wrapping_add(1);
            offset += chunk_size;
        }

        packets
    }

    /// Packetize an Annex-B H.264 access unit for UDP RTP egress.
    pub fn packetize_h264_access_unit_for_rtp(
        annex_b: &[u8],
        payload_type: u8,
        seq: &mut u16,
        ts: u32,
        ssrc: u32,
    ) -> Vec<Vec<u8>> {
        use crate::webrtc::h264_util::iter_annex_b_nal_ranges;
        let ranges = iter_annex_b_nal_ranges(annex_b);
        let mut packets = Vec::new();
        for (i, (start, end)) in ranges.iter().enumerate() {
            let marker = i + 1 == ranges.len();
            packets.extend(Self::packetize_h264_nal_for_rtp(
                &annex_b[*start..*end],
                payload_type,
                seq,
                ts,
                ssrc,
                marker,
            ));
        }
        packets
    }
}

#[derive(Debug, Clone)]
pub struct RtpHeader {
    pub version: u8,
    pub padding: u8,
    pub extension: u8,
    pub csrc_count: u8,
    pub marker: u8,
    pub payload_type: u8,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

#[derive(Debug, Clone)]
pub struct UdpTransport {
    pub client_rtp_port: u16,
    pub client_rtcp_port: u16,
    pub server_rtp_port: u16,
    pub server_rtcp_port: u16,
    pub is_tcp: bool,
}

impl UdpTransport {
    pub fn new_tcp() -> Self {
        Self {
            client_rtp_port: 0,
            client_rtcp_port: 0,
            server_rtp_port: 0,
            server_rtcp_port: 0,
            is_tcp: true,
        }
    }

    pub fn new_udp(client_rtp: u16, client_rtcp: u16, server_rtp: u16, server_rtcp: u16) -> Self {
        Self {
            client_rtp_port: client_rtp,
            client_rtcp_port: client_rtcp,
            server_rtp_port: server_rtp,
            server_rtcp_port: server_rtcp,
            is_tcp: false,
        }
    }
}

/// RFC 3640 AAC-hbr parameters from our RTSP SDP (`sizeLength=13;indexLength=3`).
const AAC_HBR_SIZE_LENGTH: u8 = 13;
const AAC_HBR_INDEX_LENGTH: u8 = 3;

/// Strip RFC 3640 `mpeg4-generic` AAC-hbr AU headers, returning raw AAC frame bytes.
pub fn strip_mpeg4_generic_aac(payload: &[u8]) -> Option<Vec<u8>> {
    if payload.is_empty() {
        return None;
    }
    // Already ADTS-framed (e.g. some pushers)
    if payload.len() >= 7 && payload[0] == 0xFF && (payload[1] & 0xF0) == 0xF0 {
        return Some(payload.to_vec());
    }
    if payload.len() < 2 {
        return None;
    }

    // First 16 bits: AU-headers-length in **bits** (RFC 3640 §3.2.1), not >> 3.
    let au_headers_length_bits = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let au_headers_length_bytes = (au_headers_length_bits + 7) / 8;
    let data_offset = 2 + au_headers_length_bytes;
    if data_offset > payload.len() || au_headers_length_bytes == 0 {
        return None;
    }

    let au_header_field_bits = AAC_HBR_SIZE_LENGTH as usize + AAC_HBR_INDEX_LENGTH as usize;
    if au_headers_length_bits % au_header_field_bits != 0 {
        return None;
    }

    let au_header_bytes = &payload[2..data_offset];
    let au_size = read_aac_hbr_au_size(au_header_bytes)?;
    if au_size == 0 {
        return None;
    }

    // Complete AU in this RTP packet (typical for ffmpeg RTSP push).
    if data_offset + au_size <= payload.len() {
        return Some(payload[data_offset..data_offset + au_size].to_vec());
    }

    // Fragmented AU: use available bytes only when this is the sole AU in the packet.
    let nb_au = au_headers_length_bits / au_header_field_bits;
    if nb_au == 1 && data_offset < payload.len() {
        return Some(payload[data_offset..].to_vec());
    }

    None
}

/// Read the first AU-size field from the AU-header section (MSB-first bit stream).
fn read_aac_hbr_au_size(au_header_bytes: &[u8]) -> Option<usize> {
    let mut bitpos = 0usize;
    let mut read_bits = |n: usize| -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte_idx = bitpos / 8;
            let bit_idx = 7 - (bitpos % 8);
            let byte = *au_header_bytes.get(byte_idx)?;
            v = (v << 1) | ((byte >> bit_idx) & 1) as u32;
            bitpos += 1;
        }
        Some(v)
    };
    let size = read_bits(AAC_HBR_SIZE_LENGTH as usize)? as usize;
    let _index = read_bits(AAC_HBR_INDEX_LENGTH as usize)?;
    Some(size)
}

#[cfg(test)]
mod aac_tests {
    use super::*;

    #[test]
    fn strip_aac_hbr_au_header() {
        // au-headers-length = 16 bits (0x0010), one 16-bit AU header, size=4 → 0x0020
        let payload = [0x00, 0x10, 0x00, 0x20, 0xDE, 0xAD, 0xBE, 0xEF];
        let stripped = strip_mpeg4_generic_aac(&payload).unwrap();
        assert_eq!(stripped, &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn strip_aac_hbr_typical_ffmpeg_length_field() {
        let au_size = 200usize;
        let au_header = ((au_size as u16) << 3) as u16;
        let mut payload = vec![0x00, 0x10, (au_header >> 8) as u8, (au_header & 0xFF) as u8];
        payload.extend(std::iter::repeat_n(0xABu8, au_size));
        let stripped = strip_mpeg4_generic_aac(&payload).unwrap();
        assert_eq!(stripped.len(), au_size);
        assert!(stripped.iter().all(|&b| b == 0xAB));
    }
}