use anyhow::Result;
use std::collections::HashMap;
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
use super::common::{
    format_rtsp_message, extract_transport, extract_track_id, is_udp_transport, RtspCommon,
};
use super::play_egress::{
    egress_rtp_packets, flush_stale_rx, prime_rtsp_play_rx, recv_coalesced_play_frame,
    PlayRtpTimeline,
};

pub struct RtspServerSession {
    reader: tokio::net::tcp::OwnedReadHalf,
    session: RtspSession,
    manager: Arc<StreamManager>,
    hls_server: Option<Arc<crate::hls::HlsServer>>,
    peer_addr: SocketAddr,
    rtp_ssrc: u32,
    write_tx: Sender<Vec<u8>>,
    running: bool,
    udp_tracks: HashMap<u8, UdpTrackTransport>,
    udp_receiver_tracks: std::collections::HashSet<u8>,
    rtp_sender_abort: Option<tokio::task::AbortHandle>,
    // H264 codec parameters
    sps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    pps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    h264_ingest: Option<H264RtpIngest>,
    // Track the session state for SDP generation
    sdp_generated: Arc<parking_lot::RwLock<bool>>,
}

#[derive(Clone)]
struct UdpTrackTransport {
    rtp_socket: Arc<tokio::net::UdpSocket>,
    rtcp_socket: Arc<tokio::net::UdpSocket>,
    client_rtp_addr: SocketAddr,
    client_rtcp_addr: SocketAddr,
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
            udp_tracks: HashMap::new(),
            udp_receiver_tracks: std::collections::HashSet::new(),
            rtp_sender_abort: None,
            sps_cache: Arc::new(parking_lot::RwLock::new(None)),
            pps_cache: Arc::new(parking_lot::RwLock::new(None)),
            h264_ingest: None,
            sdp_generated: Arc::new(parking_lot::RwLock::new(false)),
        }
    }

    async fn setup_udp_track(
        &mut self,
        track_id: u8,
        client_rtp_port: u16,
        client_rtcp_port: u16,
    ) -> Result<(u16, u16)> {
        let (server_rtp_port, server_rtcp_port) = self.allocate_udp_port_pair();

        info!(
            "[RTSP] [{}] UDP SETUP track={} client={}-{} server={}-{}",
            self.peer_addr, track_id, client_rtp_port, client_rtcp_port, server_rtp_port, server_rtcp_port
        );

        let rtp_socket = Arc::new(RtspCommon::create_udp_socket(server_rtp_port).await?);
        let rtcp_socket = Arc::new(RtspCommon::create_udp_socket(server_rtcp_port).await?);

        let client_rtp_addr = SocketAddr::new(self.peer_addr.ip(), client_rtp_port);
        let client_rtcp_addr = SocketAddr::new(self.peer_addr.ip(), client_rtcp_port);

        self.udp_tracks.insert(
            track_id,
            UdpTrackTransport {
                rtp_socket: Arc::clone(&rtp_socket),
                rtcp_socket,
                client_rtp_addr,
                client_rtcp_addr,
            },
        );

        self.session.transport_mode = TransportMode::Udp;
        if self.session.publishing {
            self.start_udp_receiver_for_track(track_id, rtp_socket);
        }
        Ok((server_rtp_port, server_rtcp_port))
    }

    fn start_udp_receiver_for_track(&mut self, track_id: u8, rtp_socket: Arc<tokio::net::UdpSocket>) {
        if !self.udp_receiver_tracks.insert(track_id) {
            return;
        }

        let stream_id = match self.session.stream_id.clone() {
            Some(id) => id,
            None => {
                self.udp_receiver_tracks.remove(&track_id);
                return;
            }
        };

        let manager = Arc::clone(&self.manager);
        let peer_addr = self.peer_addr;

        tokio::spawn(async move {
            info!(
                "[RTSP] [UDP Receiver] track={} stream='{}' from {}",
                track_id, stream_id, peer_addr
            );
            let mut buffer = vec![0u8; 65535];
            let mut h264_ingest = if track_id == 0 {
                Some(H264RtpIngest::new(manager.clone(), stream_id.clone(), "RTSP-Push-UDP"))
            } else {
                None
            };

            loop {
                match RtspCommon::receive_rtp_over_udp(&rtp_socket, &mut buffer).await {
                    Ok((len, _)) => {
                        if len < 12 || RtspCommon::is_rtcp_packet(&buffer[..len]) {
                            continue;
                        }

                        if track_id == 0 {
                            if let Some(ingest) = &mut h264_ingest {
                                ingest.ingest_rtp_packet(&buffer[..len]);
                            }
                        } else {
                            let marker = (buffer[1] & 0x80) != 0;
                            let ts = u32::from_be_bytes(
                                buffer[4..8].try_into().unwrap_or([0; 4]),
                            ) as u64;
                            let payload_type = buffer[1] & 0x7F;
                            let codec = if payload_type == 97 {
                                CodecType::AAC
                            } else {
                                CodecType::AAC
                            };
                            let media = crate::webrtc::rtp_h264_media_payload(&buffer[..len])
                                .map(|(p, _, _)| p)
                                .unwrap_or(&buffer[12..len]);
                            let aac_data = match super::common::strip_mpeg4_generic_aac(media) {
                                Some(raw) if !raw.is_empty() => raw,
                                _ => continue,
                            };
                            let frame = MediaFrame {
                                stream_id: stream_id.clone(),
                                track_id,
                                timestamp: ts,
                                data: aac_data.into(),
                                is_keyframe: marker,
                                codec,
                                rtp_data: Some(buffer[..len].to_vec().into()),
                            };
                            manager.publish_frame(frame);
                        }
                    }
                    Err(e) => {
                        error!(
                            "[RTSP] [UDP Receiver] track={} stream='{}' error: {}",
                            track_id, stream_id, e
                        );
                        break;
                    }
                }
            }
        });
    }

    async fn ensure_udp_receivers_started(&mut self) {
        for (track_id, transport) in self.udp_tracks.clone() {
            self.start_udp_receiver_for_track(track_id, Arc::clone(&transport.rtp_socket));
        }
    }
    fn allocate_udp_port_pair(&self) -> (u16, u16) {
        let base = (50000 + rand::random::<u16>() % 9998) & !1;
        (base, base + 1)
    }

    pub fn is_udp_configured(&self) -> bool {
        !self.udp_tracks.is_empty()
    }

    async fn send_rtp_over_udp_track(&self, data: &[u8], track_id: u8) -> Result<()> {
        let transport = self
            .udp_tracks
            .get(&track_id)
            .ok_or_else(|| anyhow::anyhow!("UDP track {} not configured", track_id))?;
        RtspCommon::send_rtp_over_udp(&transport.rtp_socket, data, transport.client_rtp_addr)
            .await?;
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

        let result = self.read_loop().await;
        self.cleanup_on_disconnect();

        if let Err(e) = result {
            error!("[RTSP] [{}] Session error: {}", self.peer_addr, e);
        }

        self.running = false;
        info!("[RTSP] [{}] Session handler stopped", self.peer_addr);
    }

    /// Stop PLAY egress when the client disconnects without TEARDOWN.
    fn cleanup_on_disconnect(&mut self) {
        if self.rtp_sender_abort.is_some() || self.session.rtp_task_started {
            info!(
                "[RTSP] [{}] Client disconnected — stopping PLAY RTP sender",
                self.peer_addr
            );
        }
        self.abort_rtp_sender();
        self.session.playing = false;
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

        let method = request
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().next())
            .unwrap_or("");
        let mut setup_server_ports: Option<(u16, u16)> = None;
        if method == "SETUP" {
            let transport = extract_transport(&request);
            if is_udp_transport(&transport) {
                if let Some((client_rtp, client_rtcp)) = transport.client_port {
                    let url = request
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("");
                    let track_id = extract_track_id(url) as u8;
                    match self
                        .setup_udp_track(track_id, client_rtp, client_rtcp)
                        .await
                    {
                        Ok(ports) => setup_server_ports = Some(ports),
                        Err(e) => warn!("[RTSP] [{}] UDP SETUP failed: {}", self.peer_addr, e),
                    }
                }
            }
        }

        if method == "TEARDOWN" {
            self.abort_rtp_sender();
            self.udp_tracks.clear();
        } else if method == "PAUSE" {
            self.abort_rtp_sender();
        }

        let response = RtspServer::process_rtsp_request(
            &request,
            &self.manager,
            &mut self.session,
            self.peer_addr,
            self.hls_server.clone(),
            setup_server_ports,
        )
        .await?;

        info!("[RTSP] [{}] Response #{}, length={}", self.peer_addr, request_count, response.len());
        
        let parsed_response = RtspResponse::parse(&response).unwrap_or_else(|| RtspResponse::new(500, "Internal Server Error").with_cseq("0"));
        self.send_response(&parsed_response).await?;

        if method == "RECORD" && self.session.transport_mode == TransportMode::Udp {
            self.ensure_udp_receivers_started().await;
        }

        if method == "PLAY" && self.session.playing && self.session.stream_id.is_some() {
            self.start_rtp_sender().await;
        }

        Ok(())
    }

    fn abort_rtp_sender(&mut self) {
        if let Some(handle) = self.rtp_sender_abort.take() {
            handle.abort();
        }
        self.session.rtp_task_started = false;
    }

    async fn start_rtp_sender(&mut self) {
        self.abort_rtp_sender();

        self.session.rtp_task_started = true;

        let stream_id = self.session.stream_id.clone().unwrap();
        let write_tx = self.write_tx.clone();
        let rtp_ssrc = self.rtp_ssrc;
        let manager = Arc::clone(&self.manager);
        let sps_cache = Arc::clone(&self.sps_cache);
        let pps_cache = Arc::clone(&self.pps_cache);
        let peer_addr = self.peer_addr;
        let use_udp = self.session.transport_mode == TransportMode::Udp;
        let udp_tracks = self.udp_tracks.clone();

        let handle = tokio::spawn(async move {
            info!(
                "[RTSP] [{}] Starting RTP sender (udp={}) SSRC={}",
                stream_id, use_udp, rtp_ssrc
            );

            manager.ensure_stream_broadcast(&stream_id);
            if let Some(mut rx) = manager.subscribe(&stream_id) {
                info!("[RTSP] [{}] Successfully subscribed to stream channel", stream_id);

                let mut frame_count: u64 = 0;
                let mut rtp_seq: std::collections::HashMap<u8, u16> = std::collections::HashMap::new();
                let mut timelines: std::collections::HashMap<u8, PlayRtpTimeline> =
                    std::collections::HashMap::new();
                let mut pending: Option<MediaFrame> =
                    prime_rtsp_play_rx(&mut rx, &manager, &stream_id).await;

                'rtp: loop {
                    let frame = if let Some(f) = pending.take() {
                        f
                    } else {
                        match recv_coalesced_play_frame(&mut rx).await {
                            Ok(f) => f,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                info!(
                                    "[RTSP] [{}] PLAY lagged {} frames — jump to live",
                                    stream_id, n
                                );
                                flush_stale_rx(&mut rx, &manager, &stream_id);
                                timelines.clear();
                                if let Some(idr) =
                                    prime_rtsp_play_rx(&mut rx, &manager, &stream_id).await
                                {
                                    pending = Some(idr);
                                }
                                continue;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break 'rtp,
                        }
                    };

                    frame_count += 1;
                    let seq = rtp_seq.entry(frame.track_id).or_insert(0);
                    let timeline = timelines
                        .entry(frame.track_id)
                        .or_insert_with(|| PlayRtpTimeline::for_codec(frame.codec));
                    let packets =
                        egress_rtp_packets(&frame, &manager, &stream_id, timeline, seq, rtp_ssrc);

                    if use_udp {
                        if let Some(track) = udp_tracks.get(&frame.track_id) {
                            let mut send_failed = false;
                            for packet in packets {
                                if packet.is_empty() {
                                    continue;
                                }
                                if RtspCommon::send_rtp_over_udp(
                                    &track.rtp_socket,
                                    &packet,
                                    track.client_rtp_addr,
                                )
                                .await
                                .is_err()
                                {
                                    error!(
                                        "[RTSP] [{}] Failed to send UDP RTP track={}",
                                        stream_id, frame.track_id
                                    );
                                    send_failed = true;
                                    break;
                                }
                            }
                            if send_failed {
                                break 'rtp;
                            }
                        }
                        continue;
                    }

                    // TCP interleaved: one RTP packet per interleaved frame
                    for packet in packets {
                        if packet.is_empty() {
                            continue;
                        }
                        let interleaved = RtspCommon::wrap_interleaved(&packet, frame.track_id);
                        if write_tx.send(interleaved).await.is_err() {
                            info!(
                                "[RTSP] [{}] PLAY client {} disconnected — stopping RTP sender",
                                stream_id, peer_addr
                            );
                            break 'rtp;
                        }
                    }
                    if frame_count <= 10 || frame.is_keyframe || frame_count % 100 == 0 {
                        info!(
                            "[RTSP] [{}] Sent RTP to client {}: track={}, keyframe={}, frame#={}",
                            stream_id, peer_addr, frame.track_id, frame.is_keyframe, frame_count
                        );
                    }
                }

                info!("[RTSP] [{}] RTP sender task stopped", stream_id);
            } else {
                warn!("[RTSP] [{}] Failed to subscribe to stream", stream_id);
            }
        });
        self.rtp_sender_abort = Some(handle.abort_handle());
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
        sdp.push_str("m=video 0 RTP/AVP 96\r\n");
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
        sdp.push_str("m=audio 0 RTP/AVP 97\r\n");
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