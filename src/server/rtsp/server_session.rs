use anyhow::Result;
use bytes::BytesMut;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tracing::{debug, error, info, warn};

use super::common::{
    extract_track_id, extract_transport, format_rtsp_message, is_udp_transport, RtspCommon,
};
use super::connection::RtspConnection;
use super::play_egress::{egress_rtp_packets, is_idr, prime_rtsp_play, PlayRtpTimeline};
use super::sdp_builder;
use super::{
    InterleavedChannel, RtspRequest, RtspResponse, RtspServer, RtspSessionState, TransportMode,
};
use crate::core::dispatch::DispatchError;
use crate::core::hevc_sprop_from_nals;
use crate::core::{
    CodecType, DispatchPolicy, MediaFrame, MetricsRegistry, PlaybackProtocol, StreamManager, Track,
    AAC_DEFAULT_CLOCK_RATE,
};
use crate::server::socket_transport::{spawn_write_loop, ChannelSocketWriter, SocketReader};
use crate::server::webrtc::{H264RtpIngest, HevcRtpIngest};

pub struct RtspServerSession {
    conn: RtspConnection,
    session: RtspSessionState,
    manager: Arc<StreamManager>,
    hls_server: Option<Arc<crate::server::hls::HlsServer>>,
    rtp_ssrc: u32,
    running: bool,
    udp_tracks: HashMap<u8, UdpTrackTransport>,
    udp_receiver_tracks: std::collections::HashSet<u8>,
    rtp_sender_abort: Option<tokio::task::AbortHandle>,
    vps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    sps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    pps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    h264_ingest: Option<H264RtpIngest>,
    hevc_ingest: Option<HevcRtpIngest>,
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
        hls_server: Option<Arc<crate::server::hls::HlsServer>>,
    ) -> Self {
        let session = RtspSessionState::new();
        Self {
            conn: RtspConnection::new(socket),
            session,
            manager,
            hls_server,
            rtp_ssrc: rand::random(),
            running: true,
            udp_tracks: HashMap::new(),
            udp_receiver_tracks: std::collections::HashSet::new(),
            rtp_sender_abort: None,
            vps_cache: Arc::new(parking_lot::RwLock::new(None)),
            sps_cache: Arc::new(parking_lot::RwLock::new(None)),
            pps_cache: Arc::new(parking_lot::RwLock::new(None)),
            h264_ingest: None,
            hevc_ingest: None,
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
            self.conn.peer_addr(),
            track_id,
            client_rtp_port,
            client_rtcp_port,
            server_rtp_port,
            server_rtcp_port
        );

        let rtp_socket = Arc::new(RtspCommon::create_udp_socket(server_rtp_port).await?);
        let rtcp_socket = Arc::new(RtspCommon::create_udp_socket(server_rtcp_port).await?);

        let client_rtp_addr = SocketAddr::new(self.conn.peer_addr().ip(), client_rtp_port);
        let client_rtcp_addr = SocketAddr::new(self.conn.peer_addr().ip(), client_rtcp_port);

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

    fn start_udp_receiver_for_track(
        &mut self,
        track_id: u8,
        rtp_socket: Arc<tokio::net::UdpSocket>,
    ) {
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
        let peer_addr = self.conn.peer_addr().clone();
        let session_tracks = self.session.tracks.clone();

        tokio::spawn(async move {
            info!(
                "[RTSP] [UDP Receiver] track={} stream='{}' from {}",
                track_id, stream_id, peer_addr
            );
            let mut buffer = vec![0u8; 65535];
            let track_codec = codec_for_track(&session_tracks, track_id).unwrap_or(CodecType::H264);
            let mut h264_ingest = if track_codec == CodecType::H264 {
                Some(H264RtpIngest::new(
                    manager.clone(),
                    stream_id.clone(),
                    "RTSP-Push-UDP",
                ))
            } else {
                None
            };
            let mut hevc_ingest = if track_codec == CodecType::H265 {
                Some(HevcRtpIngest::new(
                    manager.clone(),
                    stream_id.clone(),
                    "RTSP-Push-UDP",
                ))
            } else {
                None
            };

            loop {
                match RtspCommon::receive_rtp_over_udp(&rtp_socket, &mut buffer).await {
                    Ok((len, _)) => {
                        if len < 12 || RtspCommon::is_rtcp_packet(&buffer[..len]) {
                            continue;
                        }

                        match track_codec {
                            CodecType::H265 => {
                                if let Some(ingest) = &mut hevc_ingest {
                                    ingest.ingest_rtp_packet(&buffer[..len]);
                                }
                            }
                            CodecType::H264 => {
                                if let Some(ingest) = &mut h264_ingest {
                                    ingest.ingest_rtp_packet(&buffer[..len]);
                                }
                            }
                            CodecType::AAC | CodecType::Opus | CodecType::G711 => {
                                let marker = (buffer[1] & 0x80) != 0;
                                let ts =
                                    u32::from_be_bytes(buffer[4..8].try_into().unwrap_or([0; 4]))
                                        as u64;
                                let payload_type = buffer[1] & 0x7F;
                                let clock_rate = clock_rate_for_track_or_payload(
                                    &session_tracks,
                                    track_id,
                                    payload_type,
                                )
                                .unwrap_or(AAC_DEFAULT_CLOCK_RATE);
                                let media =
                                    crate::server::webrtc::rtp_h264_media_payload(&buffer[..len])
                                        .map(|(p, _, _)| p)
                                        .unwrap_or(&buffer[12..len]);
                                let frame_data = if track_codec == CodecType::AAC {
                                    match super::common::strip_mpeg4_generic_aac(media) {
                                        Some(raw) if !raw.is_empty() => raw,
                                        _ => continue,
                                    }
                                } else {
                                    media.to_vec()
                                };
                                let frame = MediaFrame {
                                    stream_id: stream_id.clone(),
                                    track_id: internal_track_id_for_codec(track_codec),
                                    timestamp: ts,
                                    clock_rate: Some(clock_rate),
                                    data: frame_data.into(),
                                    is_keyframe: marker,
                                    codec: track_codec,
                                    rtp_data: Some(buffer[..len].to_vec().into()),
                                };
                                manager.publish_frame(frame);
                            }
                            _ => {
                                debug!(
                                    "[RTSP] [UDP Receiver] ignore unsupported codec {:?} track={} stream='{}'",
                                    track_codec, track_id, stream_id
                                );
                            }
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
        self.conn.peer_addr()
    }

    pub fn session(&self) -> &RtspSessionState {
        &self.session
    }

    pub fn writer(&self) -> &ChannelSocketWriter {
        self.conn.writer()
    }

    pub async fn send_response(&mut self, response: &RtspResponse) -> Result<()> {
        self.conn.send_response(response).await
    }

    pub async fn send_rtp_packet(&self, data: &[u8]) -> Result<()> {
        self.conn.send_rtp_packet(data).await
    }

    pub async fn start(mut self) {
        info!(
            "[RTSP] [{}] Starting session handler",
            self.conn.peer_addr()
        );

        let result = self.read_loop().await;
        self.cleanup_on_disconnect();

        if let Err(e) = result {
            error!("[RTSP] [{}] Session error: {}", self.conn.peer_addr(), e);
        }

        self.running = false;
        info!("[RTSP] [{}] Session handler stopped", self.conn.peer_addr());
    }

    fn cleanup_on_disconnect(&mut self) {
        if self.rtp_sender_abort.is_some() || self.session.rtp_task_started {
            info!(
                "[RTSP] [{}] Client disconnected — stopping PLAY RTP sender",
                self.conn.peer_addr()
            );
        }
        self.abort_rtp_sender();
        if let (Some(stream_id), Some(publisher_id)) = (
            self.session.stream_id.as_deref(),
            self.session.publisher_id.as_deref(),
        ) {
            if self.manager.release_publisher(stream_id, publisher_id) {
                let _ = self.manager.set_unpublished(stream_id);
            }
        }
        self.session.playing = false;
        self.session.publishing = false;
        self.session.publisher_id = None;
    }

    async fn read_loop(&mut self) -> Result<()> {
        let mut buffer = BytesMut::with_capacity(8192);
        let mut request_count: usize = 0;

        while self.running {
            let n = self.conn.read_into(&mut buffer).await?;
            if n == 0 {
                info!(
                    "[RTSP] [{}] Connection closed by peer",
                    self.conn.peer_addr()
                );
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

        if nal_type == 28 && payload.len() >= 2 {
            let fu_indicator = payload[0];
            let fu_header = payload[1];
            let start_bit = (fu_header >> 7) & 0x01;
            let end_bit = (fu_header >> 6) & 0x01;
            let original_nal_type = fu_header & 0x1F;

            debug!("[RTSP SPS/PPS] FU-A fragment detected: start_bit={}, end_bit={}, original_nal_type={}", 
                   start_bit, end_bit, original_nal_type);

            if start_bit == 1 {
                let mut original_nal = Vec::with_capacity(1 + payload.len() - 2);
                original_nal.push((fu_indicator & 0xE0) | original_nal_type);
                original_nal.extend_from_slice(&payload[2..]);

                debug!(
                    "[RTSP SPS/PPS] Reconstructed NAL unit from FU-A start fragment: {} bytes",
                    original_nal.len()
                );

                match original_nal_type {
                    7 if sps.is_none() => {
                        info!(
                            "[RTSP SPS/PPS] Found SPS in FU-A start fragment: {} bytes, NRI={}",
                            original_nal.len(),
                            nri
                        );
                        debug!(
                            "[RTSP SPS/PPS] SPS first 16 bytes: {:02X?}",
                            &original_nal[..std::cmp::min(16, original_nal.len())]
                        );
                        sps = Some(original_nal);
                    }
                    8 if pps.is_none() => {
                        info!(
                            "[RTSP SPS/PPS] Found PPS in FU-A start fragment: {} bytes, NRI={}",
                            original_nal.len(),
                            nri
                        );
                        debug!(
                            "[RTSP SPS/PPS] PPS first 16 bytes: {:02X?}",
                            &original_nal[..std::cmp::min(16, original_nal.len())]
                        );
                        pps = Some(original_nal);
                    }
                    _ => {
                        debug!(
                            "[RTSP SPS/PPS] FU-A start fragment contains non-SPS/PPS NAL type {}",
                            original_nal_type
                        );
                    }
                }
            } else {
                debug!(
                    "[RTSP SPS/PPS] FU-A fragment is not a start fragment (start_bit=0), skipping"
                );
            }
        } else if nal_type == 24 && payload.len() >= 4 {
            debug!("[RTSP SPS/PPS] STAP-A aggregation packet detected");

            let mut offset = 1;

            while offset + 2 <= payload.len() {
                let nal_length = ((payload[offset] as usize) << 8) | (payload[offset + 1] as usize);
                offset += 2;

                if offset + nal_length <= payload.len() {
                    let nal_unit = &payload[offset..offset + nal_length];
                    let inner_nal_type = nal_unit[0] & 0x1F;
                    let inner_nri = (nal_unit[0] >> 5) & 0x03;

                    debug!(
                        "[RTSP SPS/PPS] STAP-A inner NAL: type={}, length={} bytes",
                        inner_nal_type, nal_length
                    );

                    match inner_nal_type {
                        7 if sps.is_none() => {
                            info!(
                                "[RTSP SPS/PPS] Found SPS in STAP-A: {} bytes, NRI={}",
                                nal_length, inner_nri
                            );
                            debug!(
                                "[RTSP SPS/PPS] SPS first 16 bytes: {:02X?}",
                                &nal_unit[..std::cmp::min(16, nal_unit.len())]
                            );
                            sps = Some(nal_unit.to_vec());
                        }
                        8 if pps.is_none() => {
                            info!(
                                "[RTSP SPS/PPS] Found PPS in STAP-A: {} bytes, NRI={}",
                                nal_length, inner_nri
                            );
                            debug!(
                                "[RTSP SPS/PPS] PPS first 16 bytes: {:02X?}",
                                &nal_unit[..std::cmp::min(16, nal_unit.len())]
                            );
                            pps = Some(nal_unit.to_vec());
                        }
                        _ => {
                            debug!(
                                "[RTSP SPS/PPS] STAP-A inner NAL type {} (not SPS/PPS)",
                                inner_nal_type
                            );
                        }
                    }

                    offset += nal_length;
                } else {
                    warn!("[RTSP SPS/PPS] STAP-A malformed: insufficient data for NAL unit");
                    break;
                }

                if sps.is_some() && pps.is_some() {
                    debug!("[RTSP SPS/PPS] Found both SPS and PPS, stopping STAP-A parsing");
                    break;
                }
            }
        } else if nal_type == 5 && payload.len() >= 4 {
            info!(
                "[RTSP SPS/PPS] IDR frame detected, checking for embedded SPS/PPS with start codes"
            );

            let mut offset = 0;
            while offset < payload.len() - 4 {
                if payload[offset] == 0x00
                    && payload[offset + 1] == 0x00
                    && payload[offset + 2] == 0x01
                {
                    let start_code_len = 3;
                    let nal_start = offset + start_code_len;

                    if nal_start < payload.len() {
                        let inner_nal_type = payload[nal_start] & 0x1F;
                        let inner_nri = (payload[nal_start] >> 5) & 0x03;

                        let mut next_offset = nal_start + 1;
                        while next_offset < payload.len() - 4 {
                            if payload[next_offset] == 0x00 && payload[next_offset + 1] == 0x00 {
                                if payload[next_offset + 2] == 0x01 {
                                    break;
                                } else if payload[next_offset + 2] == 0x00
                                    && payload[next_offset + 3] == 0x01
                                {
                                    next_offset += 1;
                                    break;
                                }
                            }
                            next_offset += 1;
                        }

                        let nal_unit = &payload[nal_start..next_offset];

                        debug!(
                            "[RTSP SPS/PPS] Found embedded NAL in IDR: type={}, length={} bytes",
                            inner_nal_type,
                            nal_unit.len()
                        );

                        match inner_nal_type {
                            7 if sps.is_none() => {
                                info!("[RTSP SPS/PPS] Found SPS embedded in IDR frame: {} bytes, NRI={}", 
                                      nal_unit.len(), inner_nri);
                                debug!(
                                    "[RTSP SPS/PPS] SPS first 16 bytes: {:02X?}",
                                    &nal_unit[..std::cmp::min(16, nal_unit.len())]
                                );
                                sps = Some(nal_unit.to_vec());
                            }
                            8 if pps.is_none() => {
                                info!("[RTSP SPS/PPS] Found PPS embedded in IDR frame: {} bytes, NRI={}", 
                                      nal_unit.len(), inner_nri);
                                debug!(
                                    "[RTSP SPS/PPS] PPS first 16 bytes: {:02X?}",
                                    &nal_unit[..std::cmp::min(16, nal_unit.len())]
                                );
                                pps = Some(nal_unit.to_vec());
                            }
                            _ => {}
                        }

                        offset = next_offset;
                        continue;
                    }
                } else if payload[offset] == 0x00
                    && payload[offset + 1] == 0x00
                    && payload[offset + 2] == 0x00
                    && payload[offset + 3] == 0x01
                {
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
                                } else if payload[next_offset + 2] == 0x00
                                    && payload[next_offset + 3] == 0x01
                                {
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
                            }
                            8 if pps.is_none() => {
                                info!("[RTSP SPS/PPS] Found PPS embedded in IDR frame: {} bytes, NRI={}", 
                                      nal_unit.len(), inner_nri);
                                pps = Some(nal_unit.to_vec());
                            }
                            _ => {}
                        }

                        offset = next_offset;
                        continue;
                    }
                }

                offset += 1;
            }
        } else if payload.len() >= 2 {
            info!(
                "[RTSP SPS/PPS] Single NAL unit detected: length={} bytes, nal_type={}, NRI={}",
                payload.len(),
                nal_type,
                nri
            );

            match nal_type {
                7 if sps.is_none() => {
                    info!(
                        "[RTSP SPS/PPS] Found SPS in single NAL unit: {} bytes, NRI={}",
                        payload.len(),
                        nri
                    );
                    debug!(
                        "[RTSP SPS/PPS] SPS first 16 bytes: {:02X?}",
                        &payload[..std::cmp::min(16, payload.len())]
                    );
                    sps = Some(payload.to_vec());
                }
                8 if pps.is_none() => {
                    info!(
                        "[RTSP SPS/PPS] Found PPS in single NAL unit: {} bytes, NRI={}",
                        payload.len(),
                        nri
                    );
                    debug!(
                        "[RTSP SPS/PPS] PPS first 16 bytes: {:02X?}",
                        &payload[..std::cmp::min(16, payload.len())]
                    );
                    pps = Some(payload.to_vec());
                }
                5 => {
                    info!(
                        "[RTSP SPS/PPS] Found IDR frame (nal_type=5), length={} bytes",
                        payload.len()
                    );
                }
                1 => {
                    debug!(
                        "[RTSP SPS/PPS] Found non-IDR frame (nal_type=1), length={} bytes",
                        payload.len()
                    );
                }
                6 => {
                    debug!(
                        "[RTSP SPS/PPS] Found SEI frame (nal_type=6), length={} bytes",
                        payload.len()
                    );
                }
                _ => {
                    info!("[RTSP SPS/PPS] Found unknown NAL type {} in single NAL unit, length={} bytes", nal_type, payload.len());
                }
            }
        } else {
            debug!(
                "[RTSP SPS/PPS] Payload too short ({}) bytes, cannot determine NAL type",
                payload.len()
            );
        }

        debug!(
            "[RTSP SPS/PPS] Extraction result: SPS={}, PPS={}",
            sps.is_some(),
            pps.is_some()
        );
        (sps, pps)
    }

    fn is_h264_keyframe(payload: &[u8]) -> bool {
        if payload.is_empty() {
            return false;
        }

        let first_byte = payload[0];
        let nal_type = first_byte & 0x1F;

        if nal_type == 28 && payload.len() >= 2 {
            let fu_header = payload[1];
            let original_nal_type = fu_header & 0x1F;
            return original_nal_type == 5;
        }

        nal_type == 5
    }

    fn video_codec_for_track(&self, track_id: u8) -> CodecType {
        self.session
            .tracks
            .iter()
            .find(|track| track.id == track_id)
            .map(|track| track.codec)
            .unwrap_or(CodecType::H264)
    }

    async fn handle_rtp_data(&mut self, data: &BytesMut, channel: u8) {
        if let Some(ref stream_id) = self.session.stream_id {
            if self.session.is_rtcp_channel(channel) {
                debug!(
                    "[RTSP] [{}] Ignore RTCP interleaved channel={} stream='{}'",
                    self.conn.peer_addr(),
                    channel,
                    stream_id
                );
                return;
            }

            let Some(track_id) = self.session.rtp_track_for_channel(channel) else {
                debug!(
                    "[RTSP] [{}] Ignore unknown interleaved channel={} stream='{}'",
                    self.conn.peer_addr(),
                    channel,
                    stream_id
                );
                return;
            };
            let rtp_payload = &data[4..];

            if rtp_payload.len() < 12 {
                warn!(
                    "[RTSP] [{}] RTP payload too short: {} bytes",
                    self.conn.peer_addr(),
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
            let payload_type = rtp_payload[1] & 0x7F;
            let track_codec =
                codec_for_track_or_payload(&self.session.tracks, track_id, payload_type)
                    .unwrap_or(CodecType::H264);

            match track_codec {
                CodecType::H265 => {
                    if self.hevc_ingest.is_none() {
                        self.hevc_ingest = Some(HevcRtpIngest::new(
                            Arc::clone(&self.manager),
                            stream_id.clone(),
                            "RTSP-Push",
                        ));
                    }
                    if let Some(ingest) = &mut self.hevc_ingest {
                        ingest.ingest_rtp_packet(rtp_payload);
                    }
                    self.sync_codec_cache_from_session();
                    self.sync_codec_cache_from_manager(stream_id);
                    return;
                }
                CodecType::H264 => {
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
                    self.sync_codec_cache_from_session();
                    self.sync_codec_cache_from_manager(stream_id);
                    return;
                }
                CodecType::AAC | CodecType::Opus | CodecType::G711 => {}
                _ => {
                    debug!(
                        "[RTSP] [{}] Ignore unsupported RTP codec {:?} track={} stream='{}'",
                        self.conn.peer_addr(),
                        track_codec,
                        track_id,
                        stream_id
                    );
                    return;
                }
            }

            let clock_rate =
                clock_rate_for_track_or_payload(&self.session.tracks, track_id, payload_type)
                    .unwrap_or(AAC_DEFAULT_CLOCK_RATE);
            let media_payload = crate::server::webrtc::rtp_h264_media_payload(rtp_payload)
                .map(|(p, _, _)| p)
                .unwrap_or(&rtp_payload[12..]);

            let frame_data = if track_codec == CodecType::AAC {
                match super::common::strip_mpeg4_generic_aac(media_payload) {
                    Some(raw) if !raw.is_empty() => raw,
                    _ => return,
                }
            } else {
                media_payload.to_vec()
            };

            let frame = MediaFrame {
                stream_id: stream_id.clone(),
                track_id: internal_track_id_for_codec(track_codec),
                timestamp: rtp_timestamp as u64,
                clock_rate: Some(clock_rate),
                data: frame_data.into(),
                is_keyframe: marker,
                codec: track_codec,
                rtp_data: Some(rtp_payload.to_vec().into()),
            };
            self.manager.publish_frame(frame);
        } else {
            warn!(
                "[RTSP] [{}] Received RTP data but stream_id not set, dropping packet",
                self.conn.peer_addr()
            );
        }
    }

    fn sync_codec_cache_from_session(&self) {
        if self.vps_cache.read().is_none() {
            if let Some(vps) = &self.session.vps {
                *self.vps_cache.write() = Some(vps.clone());
            }
        }
        if self.sps_cache.read().is_none() {
            if let Some(sps) = &self.session.sps {
                *self.sps_cache.write() = Some(sps.clone());
            }
        }
        if self.pps_cache.read().is_none() {
            if let Some(pps) = &self.session.pps {
                *self.pps_cache.write() = Some(pps.clone());
            }
        }
    }

    fn sync_codec_cache_from_manager(&self, stream_id: &str) {
        if let Some(stream) = self.manager.get_stream(&stream_id.to_string()) {
            if self.vps_cache.read().is_none() {
                if let Some(vps) = &stream.vps {
                    *self.vps_cache.write() = Some(vps.clone());
                }
            }
            if self.sps_cache.read().is_none() {
                if let Some(sps) = &stream.sps {
                    *self.sps_cache.write() = Some(sps.clone());
                }
            }
            if self.pps_cache.read().is_none() {
                if let Some(pps) = &stream.pps {
                    *self.pps_cache.write() = Some(pps.clone());
                }
            }
        }
    }

    async fn process_request(
        &mut self,
        request_data: &BytesMut,
        request_count: usize,
    ) -> Result<()> {
        let request = String::from_utf8_lossy(request_data).to_string();

        let cseq = request
            .lines()
            .find(|l| l.to_lowercase().starts_with("cseq:"))
            .and_then(|l| l.split(':').nth(1))
            .map(|s| s.trim())
            .unwrap_or("0");

        debug!(
            "[RTSP] [{}] Request #{}, cseq={}",
            self.conn.peer_addr(),
            request_count,
            cseq
        );
        debug!(
            "[RTSP] [{}] Received request:\n{}",
            self.conn.peer_addr(),
            format_rtsp_message(&request)
        );

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
                        Err(e) => {
                            warn!("[RTSP] [{}] UDP SETUP failed: {}", self.conn.peer_addr(), e)
                        }
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
            *self.conn.peer_addr(),
            self.hls_server.clone(),
            setup_server_ports,
        )
        .await?;

        info!(
            "[RTSP] [{}] Response #{}, length={}",
            self.conn.peer_addr(),
            request_count,
            response.len()
        );

        let parsed_response = RtspResponse::parse(&response)
            .unwrap_or_else(|| RtspResponse::new(500, "Internal Server Error").with_cseq("0"));
        self.conn.send_response(&parsed_response).await?;

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
        let write_tx = self.conn.writer().sender().clone();
        let rtp_ssrc = self.rtp_ssrc;
        let manager = Arc::clone(&self.manager);
        let sps_cache = Arc::clone(&self.sps_cache);
        let pps_cache = Arc::clone(&self.pps_cache);
        let peer_addr = self.conn.peer_addr().clone();
        let use_udp = self.session.transport_mode == TransportMode::Udp;
        let udp_tracks = self.udp_tracks.clone();
        let interleaved_channels = self.session.interleaved_channels.clone();

        MetricsRegistry::inc_player(&stream_id, PlaybackProtocol::Rtsp);
        let handle = tokio::spawn(async move {
            struct RtspPlayerGuard(String);
            impl Drop for RtspPlayerGuard {
                fn drop(&mut self) {
                    MetricsRegistry::dec_player(&self.0, PlaybackProtocol::Rtsp);
                }
            }
            let _player = RtspPlayerGuard(stream_id.clone());
            let play_started_at = std::time::Instant::now();
            let mut first_frame_sent = false;

            info!(
                "[RTSP] [{}] Starting RTP sender (udp={}) SSRC={}",
                stream_id, use_udp, rtp_ssrc
            );

            manager.ensure_stream_hub(&stream_id);
            if let Some(mut reader) =
                manager.dispatch_subscribe(&stream_id, DispatchPolicy::LiveSequential)
            {
                info!(
                    "[RTSP] [{}] Successfully subscribed to stream hub",
                    stream_id
                );

                let mut frame_count: u64 = 0;
                let mut rtp_seq: std::collections::HashMap<u8, u16> =
                    std::collections::HashMap::new();
                let mut timelines: std::collections::HashMap<u8, PlayRtpTimeline> =
                    std::collections::HashMap::new();
                let mut pending: Option<MediaFrame> =
                    prime_rtsp_play(&mut reader, &manager, &stream_id).await;
                let mut pending_batch: VecDeque<MediaFrame> = VecDeque::new();
                let mut await_video_idr = pending.is_none();

                'rtp: loop {
                    let frame = if let Some(f) = pending.take() {
                        f
                    } else if let Some(f) = pending_batch.pop_front() {
                        f
                    } else {
                        match reader.recv_batch().await {
                            Ok(frames) if !frames.is_empty() => {
                                pending_batch = frames.into();
                                continue;
                            }
                            Ok(_) => continue,
                            Err(DispatchError::Closed) => break 'rtp,
                        }
                    };

                    if await_video_idr && matches!(frame.codec, CodecType::H264 | CodecType::H265) {
                        if !is_idr(&frame) {
                            continue;
                        }
                        await_video_idr = false;
                    }

                    frame_count += 1;
                    let seq = rtp_seq.entry(frame.track_id).or_insert(0);
                    let timeline = timelines
                        .entry(frame.track_id)
                        .or_insert_with(|| PlayRtpTimeline::for_codec(frame.codec));
                    let packets =
                        egress_rtp_packets(&frame, &manager, &stream_id, timeline, seq, rtp_ssrc);

                    let mut sent_any = false;

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
                                sent_any = true;
                            }
                            if send_failed {
                                break 'rtp;
                            }
                        }
                        if !sent_any {
                            continue;
                        }
                    } else {
                        let rtp_channel =
                            rtp_channel_for_track(&interleaved_channels, frame.track_id)
                                .unwrap_or(frame.track_id);
                        for packet in packets {
                            if packet.is_empty() {
                                continue;
                            }
                            let interleaved = RtspCommon::wrap_interleaved(&packet, rtp_channel);
                            if write_tx.send(interleaved).await.is_err() {
                                info!(
                                    "[RTSP] [{}] PLAY client {} disconnected — stopping RTP sender",
                                    stream_id, peer_addr
                                );
                                break 'rtp;
                            }
                            sent_any = true;
                        }
                        if !sent_any {
                            continue;
                        }
                    }

                    if !first_frame_sent {
                        first_frame_sent = true;
                        MetricsRegistry::record_first_frame_ms(
                            &stream_id,
                            PlaybackProtocol::Rtsp,
                            play_started_at.elapsed().as_millis() as u64,
                        );
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
        self.conn.close().await;
    }

    pub fn get_sps(&self) -> Option<Vec<u8>> {
        self.sps_cache.read().clone()
    }

    pub fn get_pps(&self) -> Option<Vec<u8>> {
        self.pps_cache.read().clone()
    }

    pub fn is_sdp_ready(&self) -> bool {
        if self.video_codec_for_track(0) == CodecType::H265 {
            self.vps_cache.read().is_some()
                && self.sps_cache.read().is_some()
                && self.pps_cache.read().is_some()
        } else {
            self.sps_cache.read().is_some() && self.pps_cache.read().is_some()
        }
    }

    pub fn build_sdp_with_codec_params(&self, stream_id: &str) -> String {
        let vps = self.vps_cache.read();
        let sps = self.sps_cache.read();
        let pps = self.pps_cache.read();
        let video_codec = self.video_codec_for_track(0);

        if video_codec == CodecType::H265 {
            if let (Some(vps_data), Some(sps_data), Some(pps_data)) =
                (vps.as_ref(), sps.as_ref(), pps.as_ref())
            {
                info!(
                    "[RTSP] Generated dynamic HEVC SDP with VPS ({}), SPS ({}), PPS ({})",
                    vps_data.len(),
                    sps_data.len(),
                    pps_data.len()
                );
            } else {
                info!("[RTSP] Using default HEVC SDP (VPS/SPS/PPS not yet received)");
            }
        } else {
            if let (Some(sps_data), Some(pps_data)) = (sps.as_ref(), pps.as_ref()) {
                info!(
                    "[RTSP] Generated dynamic SDP with SPS ({}) and PPS ({})",
                    sps_data.len(),
                    pps_data.len()
                );
            } else {
                info!("[RTSP] Using default SDP (SPS/PPS not yet received)");
            }
        }

        sdp_builder::build_sdp_with_codec_params(
            stream_id,
            video_codec,
            vps.as_deref(),
            sps.as_deref(),
            pps.as_deref(),
        )
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

fn codec_for_track_or_payload(
    tracks: &[Track],
    track_id: u8,
    payload_type: u8,
) -> Option<CodecType> {
    tracks
        .iter()
        .find(|track| track.payload_type == payload_type)
        .or_else(|| tracks.iter().find(|track| track.id == track_id))
        .map(|track| track.codec)
}

fn codec_for_track(tracks: &[Track], track_id: u8) -> Option<CodecType> {
    tracks
        .iter()
        .find(|track| track.id == track_id)
        .map(|track| track.codec)
}

fn clock_rate_for_track_or_payload(
    tracks: &[Track],
    track_id: u8,
    payload_type: u8,
) -> Option<u32> {
    tracks
        .iter()
        .find(|track| track.payload_type == payload_type)
        .or_else(|| tracks.iter().find(|track| track.id == track_id))
        .map(|track| track.clock_rate)
}

fn internal_track_id_for_codec(codec: CodecType) -> u8 {
    match codec {
        CodecType::H264 | CodecType::H265 => 0,
        CodecType::AAC | CodecType::Opus | CodecType::G711 => 1,
        _ => 0,
    }
}

fn rtp_channel_for_track(channels: &[InterleavedChannel], track_id: u8) -> Option<u8> {
    channels
        .iter()
        .find(|c| c.track_id == track_id)
        .map(|c| c.rtp_channel)
}
