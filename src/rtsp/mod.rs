use anyhow::Result;
use bytes::BytesMut;
use rand::Rng;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Duration;
use tracing::{debug, error, info, warn};

use crate::core::{CodecType, MediaFrame, StreamManager, StreamProtocol, StreamSourceMode, Track};

pub mod client_session;
pub mod common;
pub mod messages;
pub mod play_egress;
mod puller;
pub mod pusher;
pub mod server_session;
pub mod session;

pub use client_session::RtspClientSession;
pub use common::format_rtsp_message;
pub use common::{
    extract_stream_id, extract_track_id, extract_transport, is_udp_transport,
    parse_transport_server_ports, url_prefers_udp, TransportInfo,
};
pub use common::{RtpHeader, RtspCommon, UdpTransport};
pub use messages::{RtspRequest, RtspResponse};
pub use puller::RtspPuller;
pub use pusher::RtspPusher;
pub use server_session::RtspServerSession;
pub use session::{RtspSession, TransportMode};

pub struct RtspServer {
    stream_manager: Arc<StreamManager>,
    port: u16,
    hls_server: Option<Arc<crate::hls::HlsServer>>,
}

impl RtspServer {
    pub fn new(
        stream_manager: Arc<StreamManager>,
        port: u16,
        hls_server: Option<Arc<crate::hls::HlsServer>>,
    ) -> Self {
        Self {
            stream_manager,
            port,
            hls_server,
        }
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
                    let hls = self.hls_server.clone();
                    tokio::spawn(async move {
                        let session = RtspServerSession::new(socket, manager, hls);
                        session.start().await;
                    });
                }
                Err(e) => {
                    error!("[RTSP] Accept error: {}", e);
                }
            }
        }
    }

    pub async fn process_rtsp_request(
        request: &str,
        manager: &StreamManager,
        session: &mut RtspSession,
        peer_addr: std::net::SocketAddr,
        hls_server: Option<Arc<crate::hls::HlsServer>>,
        setup_server_ports: Option<(u16, u16)>,
    ) -> Result<String> {
        let lines: Vec<&str> = request.lines().collect();
        if lines.is_empty() {
            warn!(
                "[RTSP] [{}] Empty request received, returning 400 Bad Request",
                peer_addr
            );
            return Ok(Self::build_error_response(400, "Bad Request", "0"));
        }

        let first_line = lines[0];
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() < 3 {
            warn!(
                "[RTSP] [{}] Invalid request line: {}",
                peer_addr, first_line
            );
            return Ok(Self::build_error_response(400, "Bad Request", "0"));
        }

        let method = parts[0];
        let url = parts[1];
        let cseq = Self::extract_cseq(request);

        info!(
            "[RTSP] [{}] Received request: {} {} (cseq={})",
            peer_addr, method, url, cseq
        );

        match method {
            "OPTIONS" => {
                info!("[RTSP] [{}] Handling OPTIONS request", peer_addr);
                let response = Self::build_options_response(cseq);
                Ok(response)
            }
            "DESCRIBE" => {
                info!(
                    "[RTSP] [{}] Handling DESCRIBE request, cseq={}",
                    peer_addr, cseq
                );

                let stream_id = extract_stream_id(url);
                info!("[RTSP] [{}] DESCRIBE stream_id={}", peer_addr, stream_id);

                if manager.get_stream(&stream_id.to_string()).is_none() {
                    warn!(
                        "[RTSP] [{}] Stream {} not found for DESCRIBE",
                        peer_addr, stream_id
                    );
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
                info!(
                    "[RTSP] [{}] Handling SETUP request, cseq={}",
                    peer_addr, cseq
                );

                if session.stream_id.is_none() {
                    let stream_id = extract_stream_id(url);
                    session.stream_id = Some(stream_id);
                }

                let track_id = extract_track_id(url);
                info!("[RTSP] [{}] SETUP track_id={}", peer_addr, track_id);

                if session.session_id.is_none() {
                    session.session_id = Some(rand_id());
                }

                let mut transport = extract_transport(request);
                if let Some(ports) = setup_server_ports {
                    transport.server_port = Some(ports);
                }
                info!("[RTSP] [{}] SETUP transport={:?}", peer_addr, transport);

                if transport.transport_type.to_uppercase().contains("TCP") {
                    session.transport_mode = TransportMode::Tcp;
                } else {
                    session.transport_mode = TransportMode::Udp;
                }

                if let Some((client_port, server_port)) = transport.client_port {
                    session
                        .interleaved_channels
                        .push((client_port, server_port));
                }

                let session_id = session.session_id.as_ref().unwrap();
                let response = Self::build_setup_response(cseq, session_id, &transport);
                Ok(response)
            }
            "PLAY" => {
                info!(
                    "[RTSP] [{}] Handling PLAY request, cseq={}",
                    peer_addr, cseq
                );

                if session.stream_id.is_none() {
                    warn!(
                        "[RTSP] [{}] PLAY without stream_id, returning 455",
                        peer_addr
                    );
                    return Ok(Self::build_error_response(
                        455,
                        "Method Not Valid in This State",
                        cseq,
                    ));
                }

                session.playing = true;
                let stream_id = session.stream_id.as_ref().unwrap();

                let session_id = session.session_id.clone();
                let rtp_info = Self::build_rtp_info(stream_id);
                let response = Self::build_play_response(cseq, session_id.as_deref(), &rtp_info);
                Ok(response)
            }
            "PAUSE" => {
                info!(
                    "[RTSP] [{}] Handling PAUSE request, cseq={}",
                    peer_addr, cseq
                );

                if session.stream_id.is_none() {
                    warn!("[RTSP] [{}] PAUSE without stream_id", peer_addr);
                    return Ok(Self::build_error_response(
                        455,
                        "Method Not Valid in This State",
                        cseq,
                    ));
                }

                session.playing = false;
                let response = Self::build_pause_response(cseq);
                Ok(response)
            }
            "TEARDOWN" => {
                info!(
                    "[RTSP] [{}] Handling TEARDOWN request, cseq={}",
                    peer_addr, cseq
                );

                let prev_stream_id = session.stream_id.clone();
                let prev_session_id = session.session_id.clone();
                let was_playing = session.playing;
                if let (Some(stream_id), Some(publisher_id)) = (
                    session.stream_id.as_deref(),
                    session.publisher_id.as_deref(),
                ) {
                    if manager.release_publisher(stream_id, publisher_id) {
                        let _ = manager.set_unpublished(stream_id);
                    }
                }

                session.stream_id = None;
                session.playing = false;
                session.publishing = false;
                session.publisher_id = None;
                session.interleaved_channels.clear();
                session.rtp_task_started = false;

                info!("[RTSP] [{}] TEARDOWN completed, stream_id={:?}, session_id={:?}, was_playing={}", 
                      peer_addr, prev_stream_id, prev_session_id, was_playing);

                let response = Self::build_teardown_response(cseq);
                Ok(response)
            }
            "ANNOUNCE" => {
                info!(
                    "[RTSP] [{}] Handling ANNOUNCE request, cseq={}",
                    peer_addr, cseq
                );

                let body_start = request.find("\r\n\r\n").map(|p| p + 4).unwrap_or(0);
                let body = &request[body_start..];
                debug!("[RTSP] [{}] ANNOUNCE body length={}", peer_addr, body.len());

                let stream_id = extract_stream_id(url);
                info!("[RTSP] [{}] ANNOUNCE stream_id={}", peer_addr, stream_id);

                // Log the full SDP for debugging
                debug!(
                    "[RTSP] [{}] ANNOUNCE SDP body:\n{}",
                    peer_addr,
                    format_rtsp_message(body)
                );

                // Parse SDP and extract tracks, SPS, PPS
                let (tracks, sps, pps) = RtspCommon::parse_sdp_with_sps_pps(body);
                info!(
                    "[RTSP] [{}] ANNOUNCE parsed SDP: {} tracks, SPS={}, PPS={}",
                    peer_addr,
                    tracks.len(),
                    sps.is_some(),
                    pps.is_some()
                );

                if let Some(sps_data) = &sps {
                    info!(
                        "[RTSP] [{}] ANNOUNCE SPS: {} bytes",
                        peer_addr,
                        sps_data.len()
                    );
                }
                if let Some(pps_data) = &pps {
                    info!(
                        "[RTSP] [{}] ANNOUNCE PPS: {} bytes",
                        peer_addr,
                        pps_data.len()
                    );
                }

                // Save SPS/PPS to session
                session.sps = sps.clone();
                session.pps = pps.clone();

                let tracks_to_create = if tracks.is_empty() {
                    crate::core::default_live_tracks()
                } else {
                    tracks
                };

                info!(
                    "[RTSP] [{}] ANNOUNCE creating stream with {} tracks",
                    peer_addr,
                    tracks_to_create.len()
                );
                manager.create_stream(
                    &stream_id,
                    StreamSourceMode::Push,
                    StreamProtocol::RTSP,
                    None,
                );

                let publisher_id = format!("rtsp:{}", peer_addr);
                if let Err(e) = manager.acquire_publisher(&stream_id, &publisher_id) {
                    warn!(
                        "[RTSP] [{}] Rejecting duplicate publisher for stream {}: {}",
                        peer_addr, stream_id, e
                    );
                    return Ok(Self::build_error_response(
                        409,
                        "Conflict: stream already publishing",
                        cseq,
                    ));
                }

                manager.set_stream_tracks(&stream_id, tracks_to_create.clone());

                // Must set SPS/PPS after create_stream (stream must exist in manager).
                if let (Some(sps_data), Some(pps_data)) = (&sps, &pps) {
                    manager.set_stream_sps_pps(&stream_id, sps_data.clone(), pps_data.clone());
                    info!(
                        "[RTSP] [{}] ANNOUNCE SPS/PPS saved to stream {}",
                        peer_addr, stream_id
                    );
                }

                info!(
                    "[RTSP] [{}] ANNOUNCE added {} tracks to stream",
                    peer_addr,
                    tracks_to_create.len()
                );

                let _ = manager.set_unpublished(&stream_id);

                session.stream_id = Some(stream_id.clone());
                session.publishing = true;
                session.publisher_id = Some(publisher_id);
                let response = Self::build_announce_response(cseq);
                Ok(response)
            }
            "RECORD" => {
                info!(
                    "[RTSP] [{}] Handling RECORD request, cseq={}",
                    peer_addr, cseq
                );

                let stream_id = session
                    .stream_id
                    .clone()
                    .unwrap_or_else(|| extract_stream_id(url));
                info!("[RTSP] [{}] RECORD stream_id={}", peer_addr, stream_id);

                if session.session_id.is_none() {
                    session.session_id = Some(rand_id());
                }

                manager.ensure_stream_broadcast(&stream_id);
                if session.publisher_id.is_none() {
                    let publisher_id = format!("rtsp:{}", peer_addr);
                    if let Err(e) = manager.acquire_publisher(&stream_id, &publisher_id) {
                        warn!(
                            "[RTSP] [{}] Rejecting duplicate RECORD publisher for stream {}: {}",
                            peer_addr, stream_id, e
                        );
                        return Ok(Self::build_error_response(
                            409,
                            "Conflict: stream already publishing",
                            cseq,
                        ));
                    }
                    session.publisher_id = Some(publisher_id);
                }
                let _ = manager.set_publishing(&stream_id);
                session.publishing = true;

                if let Some(hls) = hls_server {
                    if let Err(e) = hls.restart_stream(&stream_id).await {
                        warn!("[RTSP] Failed to start HLS for {}: {}", stream_id, e);
                    }
                }

                let session_id = session.session_id.as_ref().unwrap();
                let response = Self::build_record_response(cseq, session_id);
                Ok(response)
            }
            _ => {
                warn!(
                    "[RTSP] [{}] Unsupported method: {}, cseq={}",
                    peer_addr, method, cseq
                );
                Ok(Self::build_error_response(501, "Not Implemented", cseq))
            }
        }
    }

    pub fn build_rtp_packet(
        payload_type: u8,
        seq: u16,
        ts: u32,
        ssrc: u32,
        marker: bool,
        payload: &[u8],
    ) -> Vec<u8> {
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
        let mut response = RtspResponse::new(200, "OK").header("CSeq", cseq).header(
            "Public",
            "OPTIONS, DESCRIBE, SETUP, PLAY, PAUSE, TEARDOWN, ANNOUNCE, RECORD",
        );
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
        let transport_line = if transport.transport_type.to_uppercase().contains("TCP") {
            format!(
                "RTP/AVP/TCP;interleaved={}-{}",
                transport.client_port.unwrap_or((0, 1)).0,
                transport.client_port.unwrap_or((0, 1)).1
            )
        } else {
            let server_ports = transport.server_port.unwrap_or((5000, 5001));
            let client_ports = transport.client_port.unwrap_or((5000, 5001));
            format!(
                "RTP/AVP;unicast;client_port={}-{};server_port={}-{}",
                client_ports.0, client_ports.1, server_ports.0, server_ports.1
            )
        };

        let mut response = RtspResponse::new(200, "OK")
            .header("CSeq", cseq)
            .header("Session", session_id)
            .header("Transport", &transport_line);
        response.to_string()
    }

    fn build_setup_udp_response(
        cseq: &str,
        session_id: &str,
        server_rtp_port: u16,
        server_rtcp_port: u16,
        client_rtp_port: u16,
        client_rtcp_port: u16,
    ) -> String {
        let transport_line = format!(
            "RTP/AVP;client_port={}-{};server_port={}-{}",
            client_rtp_port, client_rtcp_port, server_rtp_port, server_rtcp_port
        );

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
        let mut response = RtspResponse::new(200, "OK").header("CSeq", cseq);
        response.to_string()
    }

    fn build_teardown_response(cseq: &str) -> String {
        let mut response = RtspResponse::new(200, "OK").header("CSeq", cseq);
        response.to_string()
    }

    fn build_announce_response(cseq: &str) -> String {
        let mut response = RtspResponse::new(200, "OK").header("CSeq", cseq);
        response.to_string()
    }

    fn build_record_response(cseq: &str, session_id: &str) -> String {
        let mut response = RtspResponse::new(200, "OK")
            .header("CSeq", cseq)
            .header("Session", session_id);
        response.to_string()
    }

    fn build_error_response(status_code: u32, reason: &str, cseq: &str) -> String {
        let mut response = RtspResponse::new(status_code, reason).header("CSeq", cseq);
        response.to_string()
    }

    fn build_sdp(stream_id: &str, manager: &StreamManager) -> String {
        use base64::Engine;

        let mut sdp = format!(
            "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=MediaServer Session: {}\r\nt=0 0\r\n",
            stream_id
        );

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
            format!(
                "a=fmtp:96 packetization-mode=1;profile-level-id={};sprop-parameter-sets={},{}\r\n",
                profile_level_id, sps_b64, pps_b64
            )
        } else {
            warn!(
                "[RTSP] [{}] Building SDP with default SPS/PPS (not yet received)",
                stream_id
            );
            String::from("a=fmtp:96 packetization-mode=1;profile-level-id=42E01F;sprop-parameter-sets=Z0LAHukBQBbsAAADAAQAAAMABAAAAwHNgYI=\r\n")
        };

        if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
            let tracks = if stream.tracks.is_empty() {
                crate::core::default_live_tracks()
            } else {
                stream.tracks.clone()
            };
            for (idx, track) in tracks.iter().enumerate() {
                let (media_type, codec_name, clock_rate) = match track.codec {
                    CodecType::H264 => ("video", "H264", 90000),
                    CodecType::H265 => ("video", "H265", 90000),
                    CodecType::AAC => ("audio", "mpeg4-generic", 44100),
                    CodecType::Opus => ("audio", "opus", 48000),
                    CodecType::G711 => ("audio", "PCMU", 8000),
                    _ => ("video", "H264", 90000),
                };

                sdp.push_str(&format!(
                    "m={} 0 RTP/AVP {}\r\n",
                    media_type, track.payload_type
                ));
                sdp.push_str(&format!("c=IN IP4 0.0.0.0\r\n"));
                sdp.push_str(&format!(
                    "a=rtpmap:{} {}/{}\r\n",
                    track.payload_type, codec_name, clock_rate
                ));
                sdp.push_str(&format!("a=control:trackID={}\r\n", idx));

                if track.codec == CodecType::H264 {
                    sdp.push_str(&h264_fmtp);
                } else if track.codec == CodecType::AAC {
                    sdp.push_str("a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;\r\n");
                }
            }
        } else {
            sdp.push_str("m=video 0 RTP/AVP 96\r\n");
            sdp.push_str("c=IN IP4 0.0.0.0\r\n");
            sdp.push_str("a=rtpmap:96 H264/90000\r\n");
            sdp.push_str("a=control:trackID=0\r\n");
            sdp.push_str(&h264_fmtp);

            sdp.push_str("m=audio 0 RTP/AVP 97\r\n");
            sdp.push_str("c=IN IP4 0.0.0.0\r\n");
            sdp.push_str("a=rtpmap:97 mpeg4-generic/44100/2\r\n");
            sdp.push_str("a=control:trackID=1\r\n");
            sdp.push_str("a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;\r\n");
        }

        sdp
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

    fn build_rtp_info(stream_id: &str) -> String {
        format!("url=rtsp://localhost:554/{}/trackID=0;seq=0;rtptime=0,url=rtsp://localhost:554/{}/trackID=1;seq=0;rtptime=0", stream_id, stream_id)
    }

    fn parse_sdp_tracks(sdp: &str) -> Vec<Track> {
        let mut tracks = Vec::new();
        let mut track_id = 0;

        for line in sdp.lines() {
            if line.starts_with("m=") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    let media_type = parts[0].strip_prefix("m=").unwrap_or("video");
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

fn rand_id() -> String {
    let mut rng = rand::thread_rng();
    format!("{:x}", rng.gen::<u64>())
}
