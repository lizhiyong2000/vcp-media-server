use anyhow::Result;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{Sender, Receiver, channel};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::net::SocketAddr;
use tracing::{info, warn, error, debug};
use bytes::BytesMut;

use crate::core::{StreamManager, MediaFrame, CodecType};
use crate::webrtc::H264RtpIngest;
use super::{RtspRequest, RtspResponse, RtspSession, TransportMode, RtspServer};
use super::common::RtspCommon;

fn format_rtsp_message(message: &str) -> String {
    let mut lines: Vec<String> = message
        .split("\r\n")
        .map(|line| {
            if line.is_empty() {
                "[empty line]".to_string()
            } else {
                format!("  {}", line)
            }
        })
        .collect();
    
    if lines.last() == Some(&"  [empty line]".to_string()) {
        lines.pop();
    }
    
    lines.join("\n")
}

pub struct RtspServerSession {
    reader: tokio::net::tcp::OwnedReadHalf,
    session: RtspSession,
    manager: Arc<StreamManager>,
    hls_server: Option<Arc<crate::hls::HlsServer>>,
    peer_addr: SocketAddr,
    rtp_ssrc: u32,
    write_tx: Sender<Vec<u8>>,
    running: bool,
    udp_sockets: Option<UdpSocketManager>,
    // H264 codec parameters
    sps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    pps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    h264_ingest: Option<H264RtpIngest>,
    // Track the session state for SDP generation
    sdp_generated: Arc<parking_lot::RwLock<bool>>,
}

struct UdpSocketManager {
    rtp_socket: Option<tokio::net::UdpSocket>,
    rtcp_socket: Option<tokio::net::UdpSocket>,
    client_rtp_addr: Option<SocketAddr>,
    client_rtcp_addr: Option<SocketAddr>,
}

impl RtspServerSession {
    pub fn new(
        socket: TcpStream,
        manager: Arc<StreamManager>,
        hls_server: Option<Arc<crate::hls::HlsServer>>,
    ) -> Self {
        let peer_addr = socket.peer_addr().unwrap_or_else(|_| "127.0.0.1:0".parse().unwrap());
        let (reader, writer) = socket.into_split();
        let (write_tx, write_rx) = channel(100);

        tokio::spawn(async move {
            Self::write_loop(writer, write_rx).await;
        });

        Self {
            reader,
            session: RtspSession::new(),
            manager,
            hls_server,
            peer_addr,
            rtp_ssrc: rand::random(),
            write_tx,
            running: true,
            udp_sockets: None,
            sps_cache: Arc::new(parking_lot::RwLock::new(None)),
            pps_cache: Arc::new(parking_lot::RwLock::new(None)),
            h264_ingest: None,
            sdp_generated: Arc::new(parking_lot::RwLock::new(false)),
        }
    }

    pub async fn setup_udp_transport(&mut self, client_rtp_port: u16, client_rtcp_port: u16) -> Result<(u16, u16)> {
        let server_rtp_port = self.allocate_udp_port();
        let server_rtcp_port = self.allocate_udp_port();
        
        info!("[RTSP] [{}] Setting up UDP transport: client_rtp={}, client_rtcp={}, server_rtp={}, server_rtcp={}", 
              self.peer_addr, client_rtp_port, client_rtcp_port, server_rtp_port, server_rtcp_port);
        
        let rtp_socket = RtspCommon::create_udp_socket(server_rtp_port).await?;
        let rtcp_socket = RtspCommon::create_udp_socket(server_rtcp_port).await?;
        
        let client_rtp_addr: SocketAddr = format!("{}:{}", self.peer_addr.ip(), client_rtp_port).parse().unwrap();
        let client_rtcp_addr: SocketAddr = format!("{}:{}", self.peer_addr.ip(), client_rtcp_port).parse().unwrap();
        
        self.udp_sockets = Some(UdpSocketManager {
            rtp_socket: Some(rtp_socket),
            rtcp_socket: Some(rtcp_socket),
            client_rtp_addr: Some(client_rtp_addr),
            client_rtcp_addr: Some(client_rtcp_addr),
        });
        
        self.session.transport_mode = TransportMode::Udp;
        
        Ok((server_rtp_port, server_rtcp_port))
    }

    pub async fn start_udp_receiver(&mut self) {
        let Some(ref mut udp_mgr) = self.udp_sockets else {
            warn!("[RTSP] [{}] UDP sockets not configured", self.peer_addr);
            return;
        };
        
        let Some(rtp_socket) = udp_mgr.rtp_socket.take() else {
            return;
        };
        
        let stream_id = self.session.stream_id.clone().unwrap_or_default();
        let manager = Arc::clone(&self.manager);
        let peer_addr = self.peer_addr;
        
        tokio::spawn(async move {
            info!("[RTSP] [UDP Receiver] Starting UDP RTP receiver for stream {}", stream_id);
            
            let mut buffer = vec![0u8; 65535];
            let mut total_packets: u64 = 0;
            let mut total_bytes: u64 = 0;
            
            loop {
                match RtspCommon::receive_rtp_over_udp(&rtp_socket, &mut buffer).await {
                    Ok((len, src)) => {
                        total_packets += 1;
                        total_bytes += len as u64;
                        
                        if RtspCommon::is_rtcp_packet(&buffer[..len]) {
                            debug!("[RTSP] [UDP Receiver] Received RTCP packet from {} ({} bytes)", src, len);
                            continue;
                        }
                        
                        if let Some(header) = RtspCommon::parse_rtp_header(&buffer[..len]) {
                            let track_id = if header.payload_type == 96 { 0 } else { 1 };
                            let codec = if header.payload_type == 96 { CodecType::H264 } else { CodecType::AAC };
                            let is_keyframe = header.marker != 0;
                            
                            debug!("[RTSP] [UDP Receiver] RTP: track={}, seq={}, ts={}, len={}", 
                                   track_id, header.sequence_number, header.timestamp, len);
                            
                            let frame = MediaFrame {
                                stream_id: stream_id.clone(),
                                track_id,
                                timestamp: header.timestamp as u64,
                                data: buffer[12..len].to_vec().into(),
                                is_keyframe,
                                codec,
                                rtp_data: Some(buffer[..len].to_vec().into()),
                            };
                            
                            manager.publish_frame(frame);
                        }
                        
                        if total_packets % 1000 == 0 {
                            info!("[RTSP] [UDP Receiver] Stats: packets={}, bytes={}", total_packets, total_bytes);
                        }
                    }
                    Err(e) => {
                        error!("[RTSP] [UDP Receiver] Error receiving: {}", e);
                        break;
                    }
                }
            }
            
            info!("[RTSP] [UDP Receiver] Stopped: packets={}, bytes={}", total_packets, total_bytes);
        });
    }

    fn allocate_udp_port(&self) -> u16 {
        (50000 + rand::random::<u16>() % 10000) & !1
    }

    pub fn is_udp_configured(&self) -> bool {
        self.udp_sockets.is_some()
    }

    pub async fn send_rtp_over_udp(&self, data: &[u8], track_id: u8) -> Result<()> {
        let Some(ref udp_mgr) = self.udp_sockets else {
            return Err(anyhow::anyhow!("UDP not configured"));
        };
        
        let Some(ref client_addr) = udp_mgr.client_rtp_addr else {
            return Err(anyhow::anyhow!("Client address not set"));
        };
        
        let Some(ref socket) = udp_mgr.rtp_socket else {
            return Err(anyhow::anyhow!("RTP socket not available"));
        };
        
        RtspCommon::send_rtp_over_udp(socket, data, *client_addr).await?;
        Ok(())
    }

    pub fn peer_addr(&self) -> &SocketAddr {
        &self.peer_addr
    }

    pub fn session(&self) -> &RtspSession {
        &self.session
    }

    pub fn write_sender(&self) -> Sender<Vec<u8>> {
        self.write_tx.clone()
    }

    pub async fn send_response(&self, response: &RtspResponse) -> Result<()> {
        let data = response.to_string().into_bytes();
        
        // Log the response in a human-readable format
        let response_str = response.to_string();
        debug!("[RTSP] [{}] Sending response:\n{}", self.peer_addr, format_rtsp_message(&response_str));
        
        self.write_tx.send(data).await?;
        Ok(())
    }

    pub async fn send_rtp_packet(&self, data: &[u8]) -> Result<()> {
        self.write_tx.send(data.to_vec()).await?;
        Ok(())
    }

    pub async fn start(mut self) {
        info!("[RTSP] [{}] Starting session handler", self.peer_addr);

        if let Err(e) = self.read_loop().await {
            error!("[RTSP] [{}] Session error: {}", self.peer_addr, e);
        }

        self.running = false;
        info!("[RTSP] [{}] Session handler stopped", self.peer_addr);
    }

    async fn read_loop(&mut self) -> Result<()> {
        let mut buffer = BytesMut::with_capacity(8192);
        let mut request_count: usize = 0;

        while self.running {
            let n = self.reader.read_buf(&mut buffer).await?;
            if n == 0 {
                info!("[RTSP] [{}] Connection closed by peer", self.peer_addr);
                break;
            }

            while !buffer.is_empty() {
                let buf_slice = &buffer[..];

                if buf_slice.starts_with(b"$") {
                    if buf_slice.len() >= 4 {
                        let channel = buf_slice[1];
                        let length = ((buf_slice[2] as usize) << 8) | (buf_slice[3] as usize);
                        let packet_length = 4 + length;

                        if buf_slice.len() >= packet_length {
                            let rtp_data = buffer.split_to(packet_length);
                            self.handle_rtp_data(&rtp_data, channel).await;
                            continue;
                        }
                    }
                    break;
                }

                if let Some(pos) = buf_slice.windows(4).position(|w| w == b"\r\n\r\n") {
                    let content_length = self.parse_content_length(buf_slice, pos);
                    let total_length = pos + 4 + content_length;

                    if buffer.len() >= total_length {
                        request_count += 1;
                        let request_data = buffer.split_to(total_length);
                        self.process_request(&request_data, request_count).await?;
                        continue;
                    }
                }
                break;
            }
        }

        Ok(())
    }

    async fn write_loop(mut writer: tokio::net::tcp::OwnedWriteHalf, mut rx: Receiver<Vec<u8>>) {
        while let Some(data) = rx.recv().await {
            if let Err(e) = writer.write_all(&data).await {
                error!("[RTSP] Write error: {}", e);
                break;
            }
            if let Err(e) = writer.flush().await {
                error!("[RTSP] Flush error: {}", e);
                break;
            }
        }
        info!("[RTSP] Write loop stopped");
    }

    // Extract SPS/PPS from H264 RTP payload
    // Handles:
    // 1. Single NAL units (SPS type 7, PPS type 8)
    // 2. FU-A fragmentation (type 28)
    // 3. STAP-A aggregation (type 24)
    // 4. IDR frames containing embedded SPS/PPS with start codes
    fn extract_h264_sps_pps_from_payload(payload: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
        let mut sps: Option<Vec<u8>> = None;
        let mut pps: Option<Vec<u8>> = None;

        if payload.is_empty() {
            debug!("[RTSP SPS/PPS] Payload is empty, returning None");
            return (None, None);
        }

        let first_byte = payload[0];
        let nal_type = first_byte & 0x1F;
        let nri = (first_byte >> 5) & 0x03;

        debug!("[RTSP SPS/PPS] Analyzing payload: length={} bytes, first_byte=0x{:02X}, nal_type={}, NRI={}", 
               payload.len(), first_byte, nal_type, nri);

        // Check for FU-A fragmentation (type 28)
        if nal_type == 28 && payload.len() >= 2 {
            // FU-A format: FU indicator + FU header
            let fu_indicator = payload[0];
            let fu_header = payload[1];
            let start_bit = (fu_header >> 7) & 0x01;
            let end_bit = (fu_header >> 6) & 0x01;
            let original_nal_type = fu_header & 0x1F;

            debug!("[RTSP SPS/PPS] FU-A fragment detected: start_bit={}, end_bit={}, original_nal_type={}", 
                   start_bit, end_bit, original_nal_type);

            // Only process if this is the start of a fragment
            if start_bit == 1 {
                // Reconstruct the original NAL unit
                let mut original_nal = Vec::with_capacity(1 + payload.len() - 2);
                original_nal.push((fu_indicator & 0xE0) | original_nal_type);
                original_nal.extend_from_slice(&payload[2..]);

                debug!("[RTSP SPS/PPS] Reconstructed NAL unit from FU-A start fragment: {} bytes", original_nal.len());

                // Check if it's SPS or PPS
                match original_nal_type {
                    7 if sps.is_none() => {
                        info!("[RTSP SPS/PPS] Found SPS in FU-A start fragment: {} bytes, NRI={}", 
                              original_nal.len(), nri);
                        debug!("[RTSP SPS/PPS] SPS first 16 bytes: {:02X?}", &original_nal[..std::cmp::min(16, original_nal.len())]);
                        sps = Some(original_nal);
                    },
                    8 if pps.is_none() => {
                        info!("[RTSP SPS/PPS] Found PPS in FU-A start fragment: {} bytes, NRI={}", 
                              original_nal.len(), nri);
                        debug!("[RTSP SPS/PPS] PPS first 16 bytes: {:02X?}", &original_nal[..std::cmp::min(16, original_nal.len())]);
                        pps = Some(original_nal);
                    },
                    _ => {
                        debug!("[RTSP SPS/PPS] FU-A start fragment contains non-SPS/PPS NAL type {}", original_nal_type);
                    }
                }
            } else {
                debug!("[RTSP SPS/PPS] FU-A fragment is not a start fragment (start_bit=0), skipping");
            }
        }
        // Check for STAP-A aggregation (type 24)
        else if nal_type == 24 && payload.len() >= 4 {
            debug!("[RTSP SPS/PPS] STAP-A aggregation packet detected");
            
            let mut offset = 1; // Skip STAP-A header byte
            
            while offset + 2 <= payload.len() {
                // Read 2-byte length prefix
                let nal_length = ((payload[offset] as usize) << 8) | (payload[offset + 1] as usize);
                offset += 2;
                
                if offset + nal_length <= payload.len() {
                    let nal_unit = &payload[offset..offset + nal_length];
                    let inner_nal_type = nal_unit[0] & 0x1F;
                    let inner_nri = (nal_unit[0] >> 5) & 0x03;
                    
                    debug!("[RTSP SPS/PPS] STAP-A inner NAL: type={}, length={} bytes", inner_nal_type, nal_length);
                    
                    match inner_nal_type {
                        7 if sps.is_none() => {
                            info!("[RTSP SPS/PPS] Found SPS in STAP-A: {} bytes, NRI={}", 
                                  nal_length, inner_nri);
                            debug!("[RTSP SPS/PPS] SPS first 16 bytes: {:02X?}", &nal_unit[..std::cmp::min(16, nal_unit.len())]);
                            sps = Some(nal_unit.to_vec());
                        },
                        8 if pps.is_none() => {
                            info!("[RTSP SPS/PPS] Found PPS in STAP-A: {} bytes, NRI={}", 
                                  nal_length, inner_nri);
                            debug!("[RTSP SPS/PPS] PPS first 16 bytes: {:02X?}", &nal_unit[..std::cmp::min(16, nal_unit.len())]);
                            pps = Some(nal_unit.to_vec());
                        },
                        _ => {
                            debug!("[RTSP SPS/PPS] STAP-A inner NAL type {} (not SPS/PPS)", inner_nal_type);
                        }
                    }
                    
                    offset += nal_length;
                } else {
                    warn!("[RTSP SPS/PPS] STAP-A malformed: insufficient data for NAL unit");
                    break;
                }
                
                // Stop early if we have both SPS and PPS
                if sps.is_some() && pps.is_some() {
                    debug!("[RTSP SPS/PPS] Found both SPS and PPS, stopping STAP-A parsing");
                    break;
                }
            }
        }
        // Check for IDR frame with embedded SPS/PPS (start code format)
        else if nal_type == 5 && payload.len() >= 4 {
            info!("[RTSP SPS/PPS] IDR frame detected, checking for embedded SPS/PPS with start codes");
            
            // Search for start codes (0x000001 or 0x00000001)
            let mut offset = 0;
            while offset < payload.len() - 4 {
                // Check for 3-byte start code (0x000001)
                if payload[offset] == 0x00 && payload[offset + 1] == 0x00 && payload[offset + 2] == 0x01 {
                    let start_code_len = 3;
                    let nal_start = offset + start_code_len;
                    
                    if nal_start < payload.len() {
                        let inner_nal_type = payload[nal_start] & 0x1F;
                        let inner_nri = (payload[nal_start] >> 5) & 0x03;
                        
                        // Find next start code or end of payload
                        let mut next_offset = nal_start + 1;
                        while next_offset < payload.len() - 4 {
                            if payload[next_offset] == 0x00 && payload[next_offset + 1] == 0x00 {
                                if payload[next_offset + 2] == 0x01 {
                                    break;
                                } else if payload[next_offset + 2] == 0x00 && payload[next_offset + 3] == 0x01 {
                                    next_offset += 1;
                                    break;
                                }
                            }
                            next_offset += 1;
                        }
                        
                        let nal_unit = &payload[nal_start..next_offset];
                        
                        debug!("[RTSP SPS/PPS] Found embedded NAL in IDR: type={}, length={} bytes", inner_nal_type, nal_unit.len());
                        
                        match inner_nal_type {
                            7 if sps.is_none() => {
                                info!("[RTSP SPS/PPS] Found SPS embedded in IDR frame: {} bytes, NRI={}", 
                                      nal_unit.len(), inner_nri);
                                debug!("[RTSP SPS/PPS] SPS first 16 bytes: {:02X?}", &nal_unit[..std::cmp::min(16, nal_unit.len())]);
                                sps = Some(nal_unit.to_vec());
                            },
                            8 if pps.is_none() => {
                                info!("[RTSP SPS/PPS] Found PPS embedded in IDR frame: {} bytes, NRI={}", 
                                      nal_unit.len(), inner_nri);
                                debug!("[RTSP SPS/PPS] PPS first 16 bytes: {:02X?}", &nal_unit[..std::cmp::min(16, nal_unit.len())]);
                                pps = Some(nal_unit.to_vec());
                            },
                            _ => {}
                        }
                        
                        offset = next_offset;
                        continue;
                    }
                }
                // Check for 4-byte start code (0x00000001)
                else if payload[offset] == 0x00 && payload[offset + 1] == 0x00 && 
                        payload[offset + 2] == 0x00 && payload[offset + 3] == 0x01 {
                    let start_code_len = 4;
                    let nal_start = offset + start_code_len;
                    
                    if nal_start < payload.len() {
                        let inner_nal_type = payload[nal_start] & 0x1F;
                        let inner_nri = (payload[nal_start] >> 5) & 0x03;
                        
                        let mut next_offset = nal_start + 1;
                        while next_offset < payload.len() - 4 {
                            if payload[next_offset] == 0x00 && payload[next_offset + 1] == 0x00 {
                                if payload[next_offset + 2] == 0x01 {
                                    break;
                                } else if payload[next_offset + 2] == 0x00 && payload[next_offset + 3] == 0x01 {
                                    next_offset += 1;
                                    break;
                                }
                            }
                            next_offset += 1;
                        }
                        
                        let nal_unit = &payload[nal_start..next_offset];
                        
                        debug!("[RTSP SPS/PPS] Found embedded NAL in IDR (4-byte start code): type={}, length={} bytes", 
                               inner_nal_type, nal_unit.len());
                        
                        match inner_nal_type {
                            7 if sps.is_none() => {
                                info!("[RTSP SPS/PPS] Found SPS embedded in IDR frame: {} bytes, NRI={}", 
                                      nal_unit.len(), inner_nri);
                                sps = Some(nal_unit.to_vec());
                            },
                            8 if pps.is_none() => {
                                info!("[RTSP SPS/PPS] Found PPS embedded in IDR frame: {} bytes, NRI={}", 
                                      nal_unit.len(), inner_nri);
                                pps = Some(nal_unit.to_vec());
                            },
                            _ => {}
                        }
                        
                        offset = next_offset;
                        continue;
                    }
                }
                
                offset += 1;
            }
        }
        // Single NAL unit (non-FU-A, non-STAP-A, non-IDR with embedded)
        else if payload.len() >= 2 {
            info!("[RTSP SPS/PPS] Single NAL unit detected: length={} bytes, nal_type={}, NRI={}", 
                  payload.len(), nal_type, nri);
            
            match nal_type {
                7 if sps.is_none() => {
                    info!("[RTSP SPS/PPS] Found SPS in single NAL unit: {} bytes, NRI={}", 
                          payload.len(), nri);
                    debug!("[RTSP SPS/PPS] SPS first 16 bytes: {:02X?}", &payload[..std::cmp::min(16, payload.len())]);
                    sps = Some(payload.to_vec());
                },
                8 if pps.is_none() => {
                    info!("[RTSP SPS/PPS] Found PPS in single NAL unit: {} bytes, NRI={}", 
                          payload.len(), nri);
                    debug!("[RTSP SPS/PPS] PPS first 16 bytes: {:02X?}", &payload[..std::cmp::min(16, payload.len())]);
                    pps = Some(payload.to_vec());
                },
                5 => {
                    info!("[RTSP SPS/PPS] Found IDR frame (nal_type=5), length={} bytes", payload.len());
                },
                1 => {
                    debug!("[RTSP SPS/PPS] Found non-IDR frame (nal_type=1), length={} bytes", payload.len());
                },
                6 => {
                    debug!("[RTSP SPS/PPS] Found SEI frame (nal_type=6), length={} bytes", payload.len());
                },
                _ => {
                    info!("[RTSP SPS/PPS] Found unknown NAL type {} in single NAL unit, length={} bytes", nal_type, payload.len());
                }
            }
        } else {
            debug!("[RTSP SPS/PPS] Payload too short ({}) bytes, cannot determine NAL type", payload.len());
        }

        debug!("[RTSP SPS/PPS] Extraction result: SPS={}, PPS={}", sps.is_some(), pps.is_some());
        (sps, pps)
    }

    // Check if RTP payload contains H264 IDR frame (keyframe)
    fn is_h264_keyframe(payload: &[u8]) -> bool {
        if payload.is_empty() {
            return false;
        }

        let first_byte = payload[0];
        let nal_type = first_byte & 0x1F;

        // FU-A fragmentation
        if nal_type == 28 && payload.len() >= 2 {
            let fu_header = payload[1];
            let original_nal_type = fu_header & 0x1F;
            // IDR frame has NAL type 5
            return original_nal_type == 5;
        }

        // Single NAL unit - IDR frame has NAL type 5
        nal_type == 5
    }

    async fn handle_rtp_data(&mut self, data: &BytesMut, channel: u8) {
        if let Some(ref stream_id) = self.session.stream_id {
            let track_id = channel / 2;
            let rtp_payload = &data[4..];

            if rtp_payload.len() < 12 {
                warn!(
                    "[RTSP] [{}] RTP payload too short: {} bytes",
                    self.peer_addr,
                    rtp_payload.len()
                );
                return;
            }

            let marker = (rtp_payload[1] & 0x80) != 0;
            let rtp_timestamp = u32::from_be_bytes([
                rtp_payload[4],
                rtp_payload[5],
                rtp_payload[6],
                rtp_payload[7],
            ]);

            if track_id == 0 {
                if self.h264_ingest.is_none() {
                    self.h264_ingest = Some(H264RtpIngest::new(
                        Arc::clone(&self.manager),
                        stream_id.clone(),
                        "RTSP-Push",
                    ));
                }
                if let Some(ingest) = &mut self.h264_ingest {
                    ingest.ingest_rtp_packet(rtp_payload);
                }
                self.sync_codec_cache_from_manager(stream_id);
                return;
            }

            // Audio / other tracks: pass through as before
            let payload_type = rtp_payload[1] & 0x7F;
            let codec = if payload_type == 97 {
                CodecType::AAC
            } else {
                CodecType::AAC
            };
            let media_payload = crate::webrtc::rtp_h264_media_payload(rtp_payload)
                .map(|(p, _, _)| p)
                .unwrap_or(&rtp_payload[12..]);

            let aac_data = if codec == CodecType::AAC {
                match super::common::strip_mpeg4_generic_aac(media_payload) {
                    Some(raw) if !raw.is_empty() => raw,
                    _ => return,
                }
            } else {
                media_payload.to_vec()
            };

            let frame = MediaFrame {
                stream_id: stream_id.clone(),
                track_id: track_id as u8,
                timestamp: rtp_timestamp as u64,
                data: aac_data.into(),
                is_keyframe: marker,
                codec,
                rtp_data: Some(data.to_vec().into()),
            };
            self.manager.publish_frame(frame);
        } else {
            warn!(
                "[RTSP] [{}] Received RTP data but stream_id not set, dropping packet",
                self.peer_addr
            );
        }
    }

    fn sync_codec_cache_from_manager(&self, stream_id: &str) {
        if let Some(stream) = self.manager.get_stream(&stream_id.to_string()) {
            if let (Some(sps), Some(pps)) = (&stream.sps, &stream.pps) {
                if self.sps_cache.read().is_none() {
                    *self.sps_cache.write() = Some(sps.clone());
                }
                if self.pps_cache.read().is_none() {
                    *self.pps_cache.write() = Some(pps.clone());
                }
            }
        }
    }

    async fn process_request(&mut self, request_data: &BytesMut, request_count: usize) -> Result<()> {
        let request = String::from_utf8_lossy(request_data).to_string();

        let cseq = request
            .lines()
            .find(|l| l.to_lowercase().starts_with("cseq:"))
            .and_then(|l| l.split(':').nth(1))
            .map(|s| s.trim())
            .unwrap_or("0");

        debug!("[RTSP] [{}] Request #{}, cseq={}", self.peer_addr, request_count, cseq);
        debug!("[RTSP] [{}] Received request:\n{}", self.peer_addr, format_rtsp_message(&request));

        let response = RtspServer::process_rtsp_request(
            &request,
            &self.manager,
            &mut self.session,
            self.peer_addr,
            self.hls_server.clone(),
        )
        .await?;

        info!("[RTSP] [{}] Response #{}, length={}", self.peer_addr, request_count, response.len());
        
        let parsed_response = RtspResponse::parse(&response).unwrap_or_else(|| RtspResponse::new(500, "Internal Server Error").with_cseq("0"));
        self.send_response(&parsed_response).await?;

        if self.session.playing && self.session.stream_id.is_some() && !self.session.rtp_task_started {
            self.start_rtp_sender().await;
        }

        Ok(())
    }

    async fn start_rtp_sender(&mut self) {
        self.session.rtp_task_started = true;

        let stream_id = self.session.stream_id.clone().unwrap();
        let write_tx = self.write_tx.clone();
        let rtp_ssrc = self.rtp_ssrc;
        let manager = Arc::clone(&self.manager);
        let sps_cache = Arc::clone(&self.sps_cache);
        let pps_cache = Arc::clone(&self.pps_cache);
        let peer_addr = self.peer_addr.clone();

        tokio::spawn(async move {
            info!("[RTSP] [{}] Starting RTP sender task, SSRC={}", stream_id, rtp_ssrc);

            if let Some(mut rx) = manager.subscribe(&stream_id) {
                info!("[RTSP] [{}] Successfully subscribed to stream channel", stream_id);

                let mut frame_count: u64 = 0;

                while let Ok(frame) = rx.recv().await {
                    frame_count += 1;

                    // If frame has original RTP data, send it directly
                    if let Some(ref rtp_data) = frame.rtp_data {
                        if write_tx.send(rtp_data.to_vec()).await.is_ok() {
                            if frame_count <= 10 || frame.is_keyframe || frame_count % 100 == 0 {
                                info!("[RTSP] [{}] Sent RTP packet to client {}: track={}, len={}, keyframe={}, frame#={}", 
                                       stream_id, peer_addr, frame.track_id, rtp_data.len(), frame.is_keyframe, frame_count);
                            }
                        } else {
                            error!("[RTSP] [{}] Failed to send RTP packet", stream_id);
                            break;
                        }
                    } else {
                        // For frames without RTP data (e.g., from RTMP), build RTP packet
                        let payload_type = match frame.codec {
                            CodecType::H264 => 96,
                            CodecType::AAC => 97,
                            _ => 96,
                        };

                        let rtp_payload = if frame.codec == CodecType::H264 {
                            let mut payload = Vec::with_capacity(4 + frame.data.len());
                            payload.extend_from_slice(&(frame.data.len() as u32).to_be_bytes());
                            payload.extend_from_slice(&frame.data);
                            payload
                        } else {
                            frame.data.to_vec()
                        };

                        let rtp_data = RtspCommon::build_rtp_packet(
                            payload_type,
                            frame_count as u16,
                            frame.timestamp as u32,
                            rtp_ssrc,
                            frame.is_keyframe,
                            &rtp_payload
                        );
                        let interleaved = RtspCommon::wrap_interleaved(&rtp_data, frame.track_id);
                        let len = interleaved.len();

                        if write_tx.send(interleaved).await.is_ok() {
                            if frame_count <= 10 || frame.is_keyframe || frame_count % 100 == 0 {
                                info!("[RTSP] [{}] Built and sent RTP packet to client {}: track={}, len={}, keyframe={}, frame#={}", 
                                       stream_id, peer_addr, frame.track_id, len, frame.is_keyframe, frame_count);
                            }
                        } else {
                            error!("[RTSP] [{}] Failed to send RTP packet", stream_id);
                            break;
                        }
                    }
                }

                info!("[RTSP] [{}] RTP sender task stopped", stream_id);
            } else {
                warn!("[RTSP] [{}] Failed to subscribe to stream", stream_id);
            }
        });
    }

    pub async fn close(&mut self) {
        self.running = false;
        drop(self.write_tx.clone());
    }

    pub fn get_sps(&self) -> Option<Vec<u8>> {
        self.sps_cache.read().clone()
    }

    pub fn get_pps(&self) -> Option<Vec<u8>> {
        self.pps_cache.read().clone()
    }

    pub fn is_sdp_ready(&self) -> bool {
        self.sps_cache.read().is_some() && self.pps_cache.read().is_some()
    }

    pub fn build_sdp_with_codec_params(&self, stream_id: &str) -> String {
        let sps = self.sps_cache.read();
        let pps = self.pps_cache.read();
        
        let mut sdp = format!("v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=MediaServer Session: {}\r\nt=0 0\r\n", stream_id);
        
        // Video track (H264)
        sdp.push_str("m=video 0 RTP/AVP/TCP 96\r\n");
        sdp.push_str("c=IN IP4 0.0.0.0\r\n");
        sdp.push_str("a=rtpmap:96 H264/90000\r\n");
        sdp.push_str("a=control:trackID=0\r\n");
        
        if let (Some(sps_data), Some(pps_data)) = (sps.as_ref(), pps.as_ref()) {
            // Base64 encode SPS and PPS
            use base64::Engine;
            let sps_b64 = base64::engine::general_purpose::STANDARD.encode(sps_data);
            let pps_b64 = base64::engine::general_purpose::STANDARD.encode(pps_data);
            
            // Extract profile-level-id from SPS (bytes 1-3)
            let profile_level_id = if sps_data.len() >= 4 {
                format!("{:02X}{:02X}{:02X}", sps_data[1], sps_data[2], sps_data[3])
            } else {
                "42E01F".to_string()
            };
            
            sdp.push_str(&format!("a=fmtp:96 packetization-mode=1;profile-level-id={};sprop-parameter-sets={},{}\r\n", 
                profile_level_id, sps_b64, pps_b64));
            
            info!("[RTSP] Generated dynamic SDP with SPS ({}) and PPS ({})", sps_data.len(), pps_data.len());
        } else {
            // Fallback to default values
            sdp.push_str("a=fmtp:96 packetization-mode=1;profile-level-id=42E01F;sprop-parameter-sets=Z0LAHukBQBbsAAADAAQAAAMABAAAAwHNgYI=\r\n");
            info!("[RTSP] Using default SDP (SPS/PPS not yet received)");
        }
        
        // Audio track (AAC)
        sdp.push_str("m=audio 0 RTP/AVP/TCP 97\r\n");
        sdp.push_str("c=IN IP4 0.0.0.0\r\n");
        sdp.push_str("t=0 0\r\n");
        sdp.push_str("a=rtpmap:97 mpeg4-generic/44100/2\r\n");
        sdp.push_str("a=control:trackID=1\r\n");
        sdp.push_str("a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3\r\n");
        
        sdp
    }

    fn parse_content_length(&self, buf: &[u8], header_end: usize) -> usize {
        let header_str = String::from_utf8_lossy(&buf[..header_end]);
        header_str
            .lines()
            .find(|l| l.to_lowercase().starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1))
            .map(|s| s.trim().parse::<usize>().unwrap_or(0))
            .unwrap_or(0)
    }
}