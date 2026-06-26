use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Duration;
use tracing::{info, warn, error, debug};
use bytes::BytesMut;
use rand::Rng;

use crate::core::{StreamManager, Track, CodecType, MediaFrame, StreamSourceMode, StreamProtocol};

pub mod messages;
pub mod session;
pub mod common;
pub mod client_session;
pub mod server_session;
pub mod puller;
pub mod pusher;

pub use messages::{RtspRequest, RtspResponse};
pub use common::format_rtsp_message;
pub use session::{RtspSession, TransportMode};
pub use common::{RtspCommon, RtpHeader, UdpTransport};
pub use client_session::RtspClientSession;
pub use server_session::RtspServerSession;
pub use puller::RtspPuller;
pub use pusher::RtspPusher;

pub struct RtspServer {
    stream_manager: Arc<StreamManager>,
    port: u16,
}

impl RtspServer {
    pub fn new(stream_manager: Arc<StreamManager>, port: u16) -> Self {
        Self { stream_manager, port }
    }

    pub async fn start(&self) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.port);
        info!("[RTSP] Initializing RTSP server on {}", addr);

        let listener = TcpListener::bind(&addr).await?;
        info!("[RTSP] RTSP server ready on {}", addr);

        loop {
            match listener.accept().await {
                Ok((socket, peer_addr)) => {
                    info!("[RTSP] New connection from {}", peer_addr);
                    let manager = self.stream_manager.clone();
                    tokio::spawn(async move {
                        let session = RtspServerSession::new(socket, manager);
                        session.start().await;
                    });
                }
                Err(e) => {
                    error!("[RTSP] Accept error: {}", e);
                }
            }
        }
    }

    pub async fn process_rtsp_request(request: &str, manager: &StreamManager, session: &mut RtspSession, peer_addr: std::net::SocketAddr) -> Result<String> {
        let lines: Vec<&str> = request.lines().collect();
        if lines.is_empty() {
            warn!("[RTSP] [{}] Empty request received, returning 400 Bad Request", peer_addr);
            return Ok(Self::build_error_response(400, "Bad Request", "0"));
        }

        let first_line = lines[0];
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() < 3 {
            warn!("[RTSP] [{}] Invalid request line: {}", peer_addr, first_line);
            return Ok(Self::build_error_response(400, "Bad Request", "0"));
        }

        let method = parts[0];
        let url = parts[1];
        let cseq = Self::extract_cseq(request);

        info!("[RTSP] [{}] Received request: {} {} (cseq={})", peer_addr, method, url, cseq);

        match method {
            "OPTIONS" => {
                info!("[RTSP] [{}] Handling OPTIONS request", peer_addr);
                let response = Self::build_options_response(cseq);
                Ok(response)
            }
            "DESCRIBE" => {
                info!("[RTSP] [{}] Handling DESCRIBE request, cseq={}", peer_addr, cseq);
                
                let stream_id = Self::extract_stream_id(url);
                info!("[RTSP] [{}] DESCRIBE stream_id={}", peer_addr, stream_id);
                
                if manager.get_stream(&stream_id.to_string()).is_none() {
                    warn!("[RTSP] [{}] Stream {} not found for DESCRIBE", peer_addr, stream_id);
                    return Ok(Self::build_error_response(404, "Not Found", cseq));
                }

                if session.stream_id.is_none() {
                    session.stream_id = Some(stream_id.clone());
                }

                let sdp = Self::build_sdp(&stream_id, manager);
                let response = Self::build_describe_response(cseq, &sdp);
                Ok(response)
            }
            "SETUP" => {
                info!("[RTSP] [{}] Handling SETUP request, cseq={}", peer_addr, cseq);
                
                if session.stream_id.is_none() {
                    let stream_id = Self::extract_stream_id(url);
                    session.stream_id = Some(stream_id);
                }

                let track_id = Self::extract_track_id(url);
                info!("[RTSP] [{}] SETUP track_id={}", peer_addr, track_id);

                if session.session_id.is_none() {
                    session.session_id = Some(rand_id());
                }

                let transport = Self::extract_transport(request);
                info!("[RTSP] [{}] SETUP transport={:?}", peer_addr, transport);
                
                if transport.transport_type == "RTP/AVP/TCP" {
                    session.transport_mode = TransportMode::Tcp;
                } else {
                    session.transport_mode = TransportMode::Udp;
                }

                if let Some((client_port, server_port)) = transport.client_port {
                    session.interleaved_channels.push((client_port, server_port));
                }

                let session_id = session.session_id.as_ref().unwrap();
                let response = Self::build_setup_response(cseq, session_id, &transport);
                Ok(response)
            }
            "PLAY" => {
                info!("[RTSP] [{}] Handling PLAY request, cseq={}", peer_addr, cseq);
                
                if session.stream_id.is_none() {
                    warn!("[RTSP] [{}] PLAY without stream_id, returning 455", peer_addr);
                    return Ok(Self::build_error_response(455, "Method Not Valid in This State", cseq));
                }

                session.playing = true;
                let stream_id = session.stream_id.as_ref().unwrap();
                
                let session_id = session.session_id.clone();
                let rtp_info = Self::build_rtp_info(stream_id);
                let response = Self::build_play_response(cseq, session_id.as_deref(), &rtp_info);
                Ok(response)
            }
            "PAUSE" => {
                info!("[RTSP] [{}] Handling PAUSE request, cseq={}", peer_addr, cseq);
                
                if session.stream_id.is_none() {
                    warn!("[RTSP] [{}] PAUSE without stream_id", peer_addr);
                    return Ok(Self::build_error_response(455, "Method Not Valid in This State", cseq));
                }

                session.playing = false;
                let response = Self::build_pause_response(cseq);
                Ok(response)
            }
            "TEARDOWN" => {
                info!("[RTSP] [{}] Handling TEARDOWN request, cseq={}", peer_addr, cseq);
                
                let prev_stream_id = session.stream_id.clone();
                let prev_session_id = session.session_id.clone();
                let was_playing = session.playing;

                session.stream_id = None;
                session.playing = false;
                session.interleaved_channels.clear();
                session.rtp_task_started = false;
                
                info!("[RTSP] [{}] TEARDOWN completed, stream_id={:?}, session_id={:?}, was_playing={}", 
                      peer_addr, prev_stream_id, prev_session_id, was_playing);
                
                let response = Self::build_teardown_response(cseq);
                Ok(response)
            }
            "ANNOUNCE" => {
                info!("[RTSP] [{}] Handling ANNOUNCE request, cseq={}", peer_addr, cseq);
                
                let body_start = request.find("\r\n\r\n").map(|p| p + 4).unwrap_or(0);
                let body = &request[body_start..];
                debug!("[RTSP] [{}] ANNOUNCE body length={}", peer_addr, body.len());
                
                let stream_id = Self::extract_stream_id(url);
                info!("[RTSP] [{}] ANNOUNCE stream_id={}", peer_addr, stream_id);
                
                // Log the full SDP for debugging
                debug!("[RTSP] [{}] ANNOUNCE SDP body:\n{}", peer_addr, format_rtsp_message(body));
                
                // Parse SDP and extract tracks, SPS, PPS
                let (tracks, sps, pps) = RtspCommon::parse_sdp_with_sps_pps(body);
                info!("[RTSP] [{}] ANNOUNCE parsed SDP: {} tracks, SPS={}, PPS={}", 
                      peer_addr, tracks.len(), sps.is_some(), pps.is_some());
                
                if let Some(sps_data) = &sps {
                    info!("[RTSP] [{}] ANNOUNCE SPS: {} bytes", peer_addr, sps_data.len());
                }
                if let Some(pps_data) = &pps {
                    info!("[RTSP] [{}] ANNOUNCE PPS: {} bytes", peer_addr, pps_data.len());
                }
                
                // Save SPS/PPS to session
                session.sps = sps.clone();
                session.pps = pps.clone();
                
                // Save SPS/PPS to stream so it can be used in DESCRIBE responses
                if let (Some(sps_data), Some(pps_data)) = (&sps, &pps) {
                    manager.set_stream_sps_pps(&stream_id, sps_data.clone(), pps_data.clone());
                    info!("[RTSP] [{}] ANNOUNCE SPS/PPS saved to stream {}", peer_addr, stream_id);
                }
                
                let tracks_to_create = if tracks.is_empty() {
                    vec![
                        Track {
                            id: 0,
                            codec: CodecType::H264,
                            payload_type: 96,
                            clock_rate: 90000,
                            extra_params: std::collections::HashMap::new(),
                        },
                        Track {
                            id: 1,
                            codec: CodecType::AAC,
                            payload_type: 97,
                            clock_rate: 44100,
                            extra_params: std::collections::HashMap::new(),
                        },
                    ]
                } else {
                    tracks
                };
                
                info!("[RTSP] [{}] ANNOUNCE creating stream with {} tracks", peer_addr, tracks_to_create.len());
                manager.create_stream(&stream_id, StreamSourceMode::Push, StreamProtocol::RTSP, None);
                manager.set_stream_tracks(&stream_id, tracks_to_create.clone());
                
                info!("[RTSP] [{}] ANNOUNCE added {} tracks to stream", peer_addr, tracks_to_create.len());
                
                let _ = manager.set_unpublished(&stream_id);

                session.stream_id = Some(stream_id.clone());
                let response = Self::build_announce_response(cseq);
                Ok(response)
            }
            "RECORD" => {
                info!("[RTSP] [{}] Handling RECORD request, cseq={}", peer_addr, cseq);
                
                let stream_id = Self::extract_stream_id(url);
                info!("[RTSP] [{}] RECORD stream_id={}", peer_addr, stream_id);
                
                if session.session_id.is_none() {
                    session.session_id = Some(rand_id());
                }
                
                let session_id = session.session_id.as_ref().unwrap();
                let response = Self::build_record_response(cseq, session_id);
                Ok(response)
            }
            _ => {
                warn!("[RTSP] [{}] Unsupported method: {}, cseq={}", peer_addr, method, cseq);
                Ok(Self::build_error_response(501, "Not Implemented", cseq))
            }
        }
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

    pub fn wrap_interleaved(data: &[u8], channel: u8) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + data.len());
        buf.push(0x24);
        buf.push(channel);
        buf.extend_from_slice(&((data.len() as u16).to_be_bytes()));
        buf.extend_from_slice(data);
        buf
    }

    fn build_options_response(cseq: &str) -> String {
        let mut response = RtspResponse::new(200, "OK")
            .header("CSeq", cseq)
            .header("Public", "OPTIONS, DESCRIBE, SETUP, PLAY, PAUSE, TEARDOWN, ANNOUNCE, RECORD");
        response.to_string()
    }

    fn build_describe_response(cseq: &str, sdp: &str) -> String {
        let mut response = RtspResponse::new(200, "OK")
            .header("CSeq", cseq)
            .header("Content-Type", "application/sdp")
            .body(sdp);
        response.to_string()
    }

    fn build_setup_response(cseq: &str, session_id: &str, transport: &TransportInfo) -> String {
        let transport_line = if transport.transport_type == "RTP/AVP/TCP" {
            format!("RTP/AVP/TCP;interleaved={}-{}", transport.client_port.unwrap_or((0, 1)).0, transport.client_port.unwrap_or((0, 1)).1)
        } else {
            let server_ports = transport.server_port.unwrap_or((5000, 5001));
            let client_ports = transport.client_port.unwrap_or((5000, 5001));
            format!("RTP/AVP;client_port={}-{};server_port={}-{}", 
                    client_ports.0, client_ports.1, server_ports.0, server_ports.1)
        };
        
        let mut response = RtspResponse::new(200, "OK")
            .header("CSeq", cseq)
            .header("Session", session_id)
            .header("Transport", &transport_line);
        response.to_string()
    }

    fn build_setup_udp_response(cseq: &str, session_id: &str, server_rtp_port: u16, server_rtcp_port: u16, client_rtp_port: u16, client_rtcp_port: u16) -> String {
        let transport_line = format!("RTP/AVP;client_port={}-{};server_port={}-{}", 
                client_rtp_port, client_rtcp_port, server_rtp_port, server_rtcp_port);
        
        info!("[RTSP] Building UDP SETUP response: {}", transport_line);
        
        let mut response = RtspResponse::new(200, "OK")
            .header("CSeq", cseq)
            .header("Session", session_id)
            .header("Transport", &transport_line);
        response.to_string()
    }

    fn build_play_response(cseq: &str, session_id: Option<&str>, rtp_info: &str) -> String {
        let mut response = RtspResponse::new(200, "OK")
            .header("CSeq", cseq)
            .header("Range", "npt=0.000-")
            .header("RTP-Info", rtp_info);
        
        if let Some(session) = session_id {
            response = response.header("Session", session);
        }
        
        response.to_string()
    }

    fn build_pause_response(cseq: &str) -> String {
        let mut response = RtspResponse::new(200, "OK")
            .header("CSeq", cseq);
        response.to_string()
    }

    fn build_teardown_response(cseq: &str) -> String {
        let mut response = RtspResponse::new(200, "OK")
            .header("CSeq", cseq);
        response.to_string()
    }

    fn build_announce_response(cseq: &str) -> String {
        let mut response = RtspResponse::new(200, "OK")
            .header("CSeq", cseq);
        response.to_string()
    }

    fn build_record_response(cseq: &str, session_id: &str) -> String {
        let mut response = RtspResponse::new(200, "OK")
            .header("CSeq", cseq)
            .header("Session", session_id);
        response.to_string()
    }

    fn build_error_response(status_code: u32, reason: &str, cseq: &str) -> String {
        let mut response = RtspResponse::new(status_code, reason)
            .header("CSeq", cseq);
        response.to_string()
    }

    fn build_sdp(stream_id: &str, manager: &StreamManager) -> String {
        use base64::Engine;
        
        let mut sdp = format!("v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=MediaServer Session: {}\r\nt=0 0\r\n", stream_id);
        
        // Get SPS/PPS from stream if available
        let (sps, pps) = if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
            (stream.sps.clone(), stream.pps.clone())
        } else {
            (None, None)
        };
        
        // Build fmtp line with SPS/PPS if available
        let h264_fmtp = if let (Some(sps_data), Some(pps_data)) = (&sps, &pps) {
            let sps_b64 = base64::engine::general_purpose::STANDARD.encode(sps_data);
            let pps_b64 = base64::engine::general_purpose::STANDARD.encode(pps_data);
            let profile_level_id = if sps_data.len() >= 4 {
                format!("{:02X}{:02X}{:02X}", sps_data[1], sps_data[2], sps_data[3])
            } else {
                "42E01F".to_string()
            };
            info!("[RTSP] [{}] Building SDP with SPS ({} bytes) and PPS ({} bytes), profile-level-id={}", 
                  stream_id, sps_data.len(), pps_data.len(), profile_level_id);
            format!("a=fmtp:96 packetization-mode=1;profile-level-id={};sprop-parameter-sets={},{}\r\n", 
                    profile_level_id, sps_b64, pps_b64)
        } else {
            warn!("[RTSP] [{}] Building SDP with default SPS/PPS (not yet received)", stream_id);
            String::from("a=fmtp:96 packetization-mode=1;profile-level-id=42E01F;sprop-parameter-sets=Z0LAHukBQBbsAAADAAQAAAMABAAAAwHNgYI=\r\n")
        };
        
        if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
            for (idx, track) in stream.tracks.iter().enumerate() {
                let (media_type, codec_name, clock_rate) = match track.codec {
                    CodecType::H264 => ("video", "H264", 90000),
                    CodecType::H265 => ("video", "H265", 90000),
                    CodecType::AAC => ("audio", "mpeg4-generic", 44100),
                    CodecType::Opus => ("audio", "opus", 48000),
                    CodecType::G711 => ("audio", "PCMU", 8000),
                    _ => ("video", "H264", 90000),
                };
                
                sdp.push_str(&format!("m={} 0 RTP/AVP/TCP {}\r\n", media_type, track.payload_type));
                sdp.push_str(&format!("c=IN IP4 0.0.0.0\r\n"));
                sdp.push_str(&format!("a=rtpmap:{} {}/{}\r\n", track.payload_type, codec_name, clock_rate));
                sdp.push_str(&format!("a=control:trackID={}\r\n", idx));
                
                if track.codec == CodecType::H264 {
                    sdp.push_str(&h264_fmtp);
                } else if track.codec == CodecType::AAC {
                    sdp.push_str("a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;\r\n");
                }
            }
        } else {
            sdp.push_str("m=video 0 RTP/AVP/TCP 96\r\n");
            sdp.push_str("c=IN IP4 0.0.0.0\r\n");
            sdp.push_str("a=rtpmap:96 H264/90000\r\n");
            sdp.push_str("a=control:trackID=0\r\n");
            sdp.push_str(&h264_fmtp);
            
            sdp.push_str("m=audio 0 RTP/AVP/TCP 97\r\n");
            sdp.push_str("c=IN IP4 0.0.0.0\r\n");
            sdp.push_str("a=rtpmap:97 mpeg4-generic/44100/2\r\n");
            sdp.push_str("a=control:trackID=1\r\n");
            sdp.push_str("a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;\r\n");
        }
        
        sdp
    }

    fn build_rtp_info(stream_id: &str) -> String {
        format!("url=rtsp://localhost:554/{}/trackID=0;seq=0;rtptime=0,url=rtsp://localhost:554/{}/trackID=1;seq=0;rtptime=0", stream_id, stream_id)
    }

    fn extract_cseq(request: &str) -> &str {
        for line in request.lines() {
            if line.starts_with("CSeq:") || line.starts_with("Cseq:") || line.starts_with("cseq:") {
                if let Some(val) = line.split(':').nth(1) {
                    return val.trim();
                }
            }
        }
        "0"
    }

    fn extract_stream_id(url: &str) -> String {
        let path = url::Url::parse(url).ok()
            .map(|u| u.path().to_string())
            .unwrap_or_else(|| url.to_string());
        
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        
        if let Some(last_part) = parts.last() {
            if last_part.starts_with("trackID=") {
                if parts.len() >= 2 {
                    return parts[parts.len() - 2].to_string();
                }
            }
            last_part.to_string()
        } else {
            "live".to_string()
        }
    }

    fn extract_track_id(url: &str) -> u32 {
        let path = url::Url::parse(url).ok()
            .map(|u| u.path().to_string())
            .unwrap_or_else(|| url.to_string());
        
        if let Some(idx) = path.find("trackID=") {
            let rest = &path[idx + 8..];
            if let Some(end) = rest.find(|c: char| !c.is_ascii_digit()) {
                rest[..end].parse().unwrap_or(0)
            } else {
                rest.parse().unwrap_or(0)
            }
        } else {
            0
        }
    }

    fn extract_transport(request: &str) -> TransportInfo {
        let mut info = TransportInfo::default();
        
        for line in request.lines() {
            if line.starts_with("Transport:") {
                let parts: Vec<&str> = line.split(';').collect();
                for part in parts {
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
        }
        
        info
    }

    fn parse_sdp_tracks(sdp: &str) -> Vec<Track> {
        let mut tracks = Vec::new();
        let mut track_id = 0;
        
        for line in sdp.lines() {
            if line.starts_with("m=") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    let media_type = parts[1];
                    let payload_type: u8 = parts[3].parse().unwrap_or(96);
                    
                    let codec = if media_type == "video" {
                        CodecType::H264
                    } else if media_type == "audio" {
                        CodecType::AAC
                    } else {
                        CodecType::H264
                    };
                    
                    let clock_rate = if media_type == "video" { 90000 } else { 44100 };
                    
                    tracks.push(Track {
                        id: track_id as u8,
                        codec,
                        payload_type,
                        clock_rate,
                        extra_params: std::collections::HashMap::new(),
                    });
                    
                    track_id += 1;
                }
            }
        }
        
        tracks
    }
}

#[derive(Debug, Default)]
struct TransportInfo {
    transport_type: String,
    client_port: Option<(u16, u16)>,
    server_port: Option<(u16, u16)>,
}

fn rand_id() -> String {
    let mut rng = rand::thread_rng();
    format!("{:x}", rng.gen::<u64>())
}