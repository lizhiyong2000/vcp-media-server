use anyhow::Result;
use bytes::BytesMut;
use std::sync::Arc;
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Duration;
use tracing::{info, warn, error, debug};
use url;

use crate::core::{StreamManager, Track, CodecType, MediaFrame};
use super::messages::RtspRequest;
use super::common::RtspCommon;

pub struct RtspClientSession {
    stream_manager: Arc<StreamManager>,
    remote_url: String,
    session_id: Option<String>,
    cseq: u32,
    udp_transport: Option<UdpTransportInfo>,
}

struct UdpTransportInfo {
    rtp_socket: tokio::net::UdpSocket,
    rtcp_socket: tokio::net::UdpSocket,
    server_rtp_addr: SocketAddr,
    server_rtcp_addr: SocketAddr,
    client_rtp_port: u16,
    client_rtcp_port: u16,
}

impl RtspClientSession {
    pub fn new(stream_manager: Arc<StreamManager>, remote_url: &str) -> Self {
        Self {
            stream_manager,
            remote_url: remote_url.to_string(),
            session_id: None,
            cseq: 1,
            udp_transport: None,
        }
    }

    pub async fn setup_udp_transport(&mut self, server_rtp_port: u16, server_rtcp_port: u16) -> Result<()> {
        let (local_rtp_port, local_rtcp_port) = self.allocate_udp_ports();
        
        info!("[RTSP Client] Setting up UDP transport: local_rtp={}, local_rtcp={}, server_rtp={}, server_rtcp={}",
              local_rtp_port, local_rtcp_port, server_rtp_port, server_rtcp_port);
        
        let rtp_socket = RtspCommon::create_udp_socket(local_rtp_port).await?;
        let rtcp_socket = RtspCommon::create_udp_socket(local_rtcp_port).await?;
        
        let server_rtp_addr: SocketAddr = format!("127.0.0.1:{}", server_rtp_port).parse().unwrap();
        let server_rtcp_addr: SocketAddr = format!("127.0.0.1:{}", server_rtcp_port).parse().unwrap();
        
        self.udp_transport = Some(UdpTransportInfo {
            rtp_socket,
            rtcp_socket,
            server_rtp_addr,
            server_rtcp_addr,
            client_rtp_port: local_rtp_port,
            client_rtcp_port: local_rtcp_port,
        });
        
        Ok(())
    }

    fn allocate_udp_ports(&self) -> (u16, u16) {
        let base = (50000 + rand::random::<u16>() % 10000) & !1;
        (base, base + 1)
    }

    pub async fn send_rtp_over_udp(&self, data: &[u8]) -> Result<usize> {
        let Some(ref udp) = self.udp_transport else {
            return Err(anyhow::anyhow!("UDP transport not configured"));
        };
        
        let sent = RtspCommon::send_rtp_over_udp(&udp.rtp_socket, data, udp.server_rtp_addr).await?;
        Ok(sent)
    }

    pub async fn receive_rtp_over_udp(&self, buffer: &mut [u8]) -> Result<usize> {
        let Some(ref udp) = self.udp_transport else {
            return Err(anyhow::anyhow!("UDP transport not configured"));
        };
        
        let (len, _) = RtspCommon::receive_rtp_over_udp(&udp.rtp_socket, buffer).await?;
        Ok(len)
    }

    pub fn get_client_udp_ports(&self) -> Option<(u16, u16)> {
        self.udp_transport.as_ref().map(|udp| (udp.client_rtp_port, udp.client_rtcp_port))
    }

    pub fn is_udp_configured(&self) -> bool {
        self.udp_transport.is_some()
    }

    pub async fn connect(&mut self) -> Result<(tokio::net::tcp::OwnedReadHalf, tokio::net::tcp::OwnedWriteHalf)> {
        let url = url::Url::parse(&self.remote_url)?;
        let host = url.host_str().ok_or_else(|| anyhow::anyhow!("Missing host in URL"))?;
        let port = url.port().unwrap_or(554);

        info!("[RTSP Client] Connecting to {}:{}", host, port);
        let socket = TcpStream::connect((host, port)).await?;
        info!("[RTSP Client] Connected successfully");

        Ok(socket.into_split())
    }

    pub async fn send_request(&mut self, writer: &mut tokio::net::tcp::OwnedWriteHalf, reader: &mut tokio::net::tcp::OwnedReadHalf, method: &str, url: &str) -> Result<String> {
        let request = RtspRequest::new(method, url)
            .header("CSeq", &self.cseq.to_string())
            .header("User-Agent", "MediaServer/1.0");
        
        if let Some(ref session) = self.session_id {
            let request = request.header("Session", session);
            RtspCommon::write_request(writer, &request).await?;
        } else {
            RtspCommon::write_request(writer, &request).await?;
        }
        
        let cseq_copy = self.cseq;
        self.cseq += 1;
        
        info!("[RTSP Client] Sent {} request (CSeq={})", method, cseq_copy);
        
        let response = RtspCommon::read_response(reader).await?;
        info!("[RTSP Client] Received {} response:\n{}", method, response.trim());
        
        self.session_id = RtspCommon::extract_session_id(&response);
        
        Ok(response)
    }

    pub async fn send_request_with_body(&mut self, writer: &mut tokio::net::tcp::OwnedWriteHalf, reader: &mut tokio::net::tcp::OwnedReadHalf, method: &str, url: &str, content_type: &str, body: &str) -> Result<String> {
        let request = RtspRequest::new(method, url)
            .header("CSeq", &self.cseq.to_string())
            .header("User-Agent", "MediaServer/1.0")
            .header("Content-Type", content_type)
            .body(body);
        
        if let Some(ref session) = self.session_id {
            let request = request.header("Session", session);
            RtspCommon::write_request(writer, &request).await?;
        } else {
            RtspCommon::write_request(writer, &request).await?;
        }
        
        let cseq_copy = self.cseq;
        self.cseq += 1;
        
        info!("[RTSP Client] Sent {} request (CSeq={}, body_len={})", method, cseq_copy, body.len());
        
        let response = RtspCommon::read_response(reader).await?;
        info!("[RTSP Client] Received {} response:\n{}", method, response.trim());
        
        self.session_id = RtspCommon::extract_session_id(&response);
        
        Ok(response)
    }

    pub async fn send_options(&mut self, writer: &mut tokio::net::tcp::OwnedWriteHalf, reader: &mut tokio::net::tcp::OwnedReadHalf) -> Result<String> {
        let url = self.remote_url.clone();
        self.send_request(writer, reader, "OPTIONS", &url).await
    }

    pub async fn send_describe(&mut self, writer: &mut tokio::net::tcp::OwnedWriteHalf, reader: &mut tokio::net::tcp::OwnedReadHalf) -> Result<String> {
        let request = RtspRequest::new("DESCRIBE", &self.remote_url)
            .header("CSeq", &self.cseq.to_string())
            .header("User-Agent", "MediaServer/1.0")
            .header("Accept", "application/sdp");
        
        if let Some(ref session) = self.session_id {
            let request = request.header("Session", session);
            RtspCommon::write_request(writer, &request).await?;
        } else {
            RtspCommon::write_request(writer, &request).await?;
        }
        
        let cseq_copy = self.cseq;
        self.cseq += 1;
        
        info!("[RTSP Client] Sent DESCRIBE request (CSeq={})", cseq_copy);
        
        let response = RtspCommon::read_response(reader).await?;
        info!("[RTSP Client] Received DESCRIBE response:\n{}", response.trim());
        
        Ok(response)
    }

    pub async fn send_setup(&mut self, writer: &mut tokio::net::tcp::OwnedWriteHalf, reader: &mut tokio::net::tcp::OwnedReadHalf, track_idx: usize) -> Result<String> {
        let control_url = if self.remote_url.ends_with('/') {
            format!("{}trackID={}", self.remote_url, track_idx)
        } else {
            format!("{}/trackID={}", self.remote_url, track_idx)
        };
        
        let interleaved = format!("{}-{}", track_idx * 2, track_idx * 2 + 1);
        
        let request = RtspRequest::new("SETUP", &control_url)
            .header("CSeq", &self.cseq.to_string())
            .header("User-Agent", "MediaServer/1.0")
            .header("Transport", &format!("RTP/AVP/TCP;interleaved={}", interleaved));
        
        if let Some(ref session) = self.session_id {
            let request = request.header("Session", session);
            RtspCommon::write_request(writer, &request).await?;
        } else {
            RtspCommon::write_request(writer, &request).await?;
        }
        
        let cseq_copy = self.cseq;
        self.cseq += 1;
        
        info!("[RTSP Client] Sent SETUP request for track {} (CSeq={}, interleaved={})", track_idx, cseq_copy, interleaved);
        
        let response = RtspCommon::read_response(reader).await?;
        info!("[RTSP Client] Received SETUP response for track {}:\n{}", track_idx, response.trim());
        
        self.session_id = RtspCommon::extract_session_id(&response);
        
        Ok(response)
    }

    pub async fn send_play(&mut self, writer: &mut tokio::net::tcp::OwnedWriteHalf, reader: &mut tokio::net::tcp::OwnedReadHalf) -> Result<String> {
        let request = RtspRequest::new("PLAY", &self.remote_url)
            .header("CSeq", &self.cseq.to_string())
            .header("User-Agent", "MediaServer/1.0")
            .header("Range", "npt=0.000-");
        
        if let Some(ref session) = self.session_id {
            let request = request.header("Session", session);
            RtspCommon::write_request(writer, &request).await?;
        } else {
            RtspCommon::write_request(writer, &request).await?;
        }
        
        let cseq_copy = self.cseq;
        self.cseq += 1;
        
        info!("[RTSP Client] Sent PLAY request (CSeq={})", cseq_copy);
        
        let response = RtspCommon::read_response(reader).await?;
        info!("[RTSP Client] Received PLAY response:\n{}", response.trim());
        
        Ok(response)
    }

    pub async fn send_announce(&mut self, writer: &mut tokio::net::tcp::OwnedWriteHalf, reader: &mut tokio::net::tcp::OwnedReadHalf, sdp: &str) -> Result<String> {
        let request = RtspRequest::new("ANNOUNCE", &self.remote_url)
            .header("CSeq", &self.cseq.to_string())
            .header("User-Agent", "MediaServer/1.0")
            .header("Content-Type", "application/sdp")
            .body(sdp);
        
        if let Some(ref session) = self.session_id {
            let request = request.header("Session", session);
            RtspCommon::write_request(writer, &request).await?;
        } else {
            RtspCommon::write_request(writer, &request).await?;
        }
        
        let cseq_copy = self.cseq;
        self.cseq += 1;
        
        info!("[RTSP Client] Sent ANNOUNCE request (CSeq={}, sdp_len={})", cseq_copy, sdp.len());
        
        let response = RtspCommon::read_response(reader).await?;
        info!("[RTSP Client] Received ANNOUNCE response:\n{}", response.trim());
        
        Ok(response)
    }

    pub async fn send_record(&mut self, writer: &mut tokio::net::tcp::OwnedWriteHalf, reader: &mut tokio::net::tcp::OwnedReadHalf) -> Result<String> {
        let request = RtspRequest::new("RECORD", &self.remote_url)
            .header("CSeq", &self.cseq.to_string())
            .header("User-Agent", "MediaServer/1.0")
            .header("Session", self.session_id.as_ref().unwrap_or(&"".to_string()));
        
        RtspCommon::write_request(writer, &request).await?;
        
        let cseq_copy = self.cseq;
        self.cseq += 1;
        
        info!("[RTSP Client] Sent RECORD request (CSeq={})", cseq_copy);
        
        let response = RtspCommon::read_response(reader).await?;
        info!("[RTSP Client] Received RECORD response:\n{}", response.trim());
        
        Ok(response)
    }

    pub fn session_id(&self) -> Option<&String> {
        self.session_id.as_ref()
    }

    pub fn remote_url(&self) -> &str {
        &self.remote_url
    }

    pub async fn start_keepalive(&self, mut reader: tokio::net::tcp::OwnedReadHalf) {
        let session_id = self.session_id.clone().unwrap_or_default();
        
        tokio::spawn(async move {
            let mut response_buffer = String::new();
            let mut keepalive_count = 0;
            
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                keepalive_count += 1;
                
                let mut buffer = [0u8; 4096];
                match reader.read(&mut buffer).await {
                    Ok(n) => {
                        if n > 0 {
                            let response = String::from_utf8_lossy(&buffer[..n]);
                            response_buffer.push_str(&response);
                            debug!("[RTSP Client] [Keepalive #{}] Read {} bytes", keepalive_count, n);
                            
                            if response_buffer.ends_with("\r\n\r\n") {
                                info!("[RTSP Client] [Keepalive #{}] Server response:\n{}", keepalive_count, response_buffer.trim());
                                response_buffer.clear();
                            }
                        } else {
                            debug!("[RTSP Client] [Keepalive #{}] Connection closed", keepalive_count);
                            break;
                        }
                    },
                    Err(e) => {
                        warn!("[RTSP Client] [Keepalive #{}] Error reading: {}", keepalive_count, e);
                        break;
                    }
                }
            }
        });
    }

    pub fn parse_sdp_tracks(sdp: &str) -> Vec<Track> {
        RtspCommon::parse_sdp_tracks(sdp)
    }

    pub fn build_sdp(tracks: &[Track]) -> String {
        RtspCommon::build_sdp(tracks)
    }

    pub fn build_rtp_packet(payload_type: u8, seq: u16, ts: u32, ssrc: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        RtspCommon::build_rtp_packet(payload_type, seq, ts, ssrc, marker, payload)
    }

    pub fn wrap_interleaved(data: &[u8], channel: u8) -> Vec<u8> {
        RtspCommon::wrap_interleaved(data, channel)
    }

    pub fn parse_interleaved(data: &[u8]) -> Option<(u8, &[u8])> {
        RtspCommon::parse_interleaved(data)
    }
}