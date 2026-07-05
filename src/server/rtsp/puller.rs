use anyhow::Result;
use async_trait::async_trait;
use bytes::BytesMut;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::time::Duration;
use tracing::{debug, error, info, warn};
use webrtc::sdp::description::session::SessionDescription;

use super::client_session::RtspClientSession;
use super::common::RtspCommon;
use super::RtspRequest;
use crate::core::{
    CodecType, MediaFrame, MediaTransport, StreamManager, StreamProtocol, StreamPuller,
    StreamSourceMode, Track, TransportStatus, AAC_DEFAULT_CLOCK_RATE, VIDEO_RTP_CLOCK_RATE,
};
use crate::server::webrtc::H264RtpIngest;

pub struct RtspPuller {
    stream_manager: Arc<StreamManager>,
    remote_url: Option<String>,
    stream_id: String,
    peer_addr: String,
    status: TransportStatus,
}

fn rtsp_clock_rate_for_track(tracks: &[Track], track_id: u8, payload_type: u8) -> u32 {
    tracks
        .iter()
        .find(|track| track.id == track_id || track.payload_type == payload_type)
        .map(|track| track.clock_rate)
        .unwrap_or(AAC_DEFAULT_CLOCK_RATE)
}

impl RtspPuller {
    pub fn new(stream_manager: Arc<StreamManager>) -> Self {
        Self {
            stream_manager,
            remote_url: None,
            stream_id: String::new(),
            peer_addr: String::new(),
            status: TransportStatus::Idle,
        }
    }

    pub fn with_url(mut self, remote_url: &str, stream_id: &str) -> Self {
        self.remote_url = Some(remote_url.to_string());
        self.stream_id = stream_id.to_string();
        self
    }

    async fn do_pull(&self, remote_url: &str, local_stream_id: &str) -> Result<()> {
        info!("[RTSP Puller] =========================================");
        info!(
            "[RTSP Puller] Starting RTSP Pull from {} to stream {}",
            remote_url, local_stream_id
        );
        info!("[RTSP Puller] =========================================");

        let mut session = RtspClientSession::new(self.stream_manager.clone(), remote_url);
        session.connect().await?;

        info!("[RTSP Puller] [Step 1/4] Sending OPTIONS...");
        let response = session.send_options().await?;
        if !response.starts_with("RTSP/1.0 200 OK") {
            return Err(anyhow::anyhow!("OPTIONS failed: {}", response));
        }

        info!("[RTSP Puller] [Step 2/4] Sending DESCRIBE...");
        let response = session.send_describe().await?;
        if !response.starts_with("RTSP/1.0 200 OK") {
            return Err(anyhow::anyhow!("DESCRIBE failed: {}", response));
        }

        let sdp_start = response.find("\r\n\r\n").map(|p| p + 4).unwrap_or(0);
        let sdp = &response[sdp_start..];
        let tracks = RtspClientSession::parse_sdp_tracks(sdp);

        if tracks.is_empty() {
            warn!("[RTSP Puller] No tracks found in SDP, using default tracks");
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
            tracks.clone()
        };

        info!(
            "[RTSP Puller] [Step 3/4] Creating stream {} with {} tracks",
            local_stream_id,
            tracks_to_create.len()
        );
        self.stream_manager.create_stream(
            local_stream_id,
            StreamSourceMode::Pull,
            StreamProtocol::RTSP,
            Some(remote_url.to_string()),
        );
        let _ = self.stream_manager.set_unpublished(local_stream_id);
        self.stream_manager.ensure_stream_broadcast(local_stream_id);

        if tracks_to_create
            .iter()
            .any(|track| track.codec == CodecType::H265)
        {
            if let Ok(sdp_parsed) = SessionDescription::try_from(sdp.to_string()) {
                for md in &sdp_parsed.media_descriptions {
                    for attr in &md.attributes {
                        if attr.key == "fmtp" {
                            if let Some(value) = &attr.value {
                                let (vps, sps, pps) = crate::core::parse_hevc_sprop_fmtp(value);
                                if sps.is_some() {
                                    self.stream_manager.set_stream_hevc_params(
                                        local_stream_id,
                                        vps.unwrap_or_default(),
                                        sps.unwrap_or_default(),
                                        pps.unwrap_or_default(),
                                    );
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        } else {
            use crate::server::webrtc::parse_sprop_parameter_sets;
            let (sdp_sps, sdp_pps) = parse_sprop_parameter_sets(sdp);
            if let (Some(sps), Some(pps)) = (sdp_sps, sdp_pps) {
                info!(
                    "[RTSP Puller] Primed SPS/PPS from SDP stream='{}' sps={} pps={}",
                    local_stream_id,
                    sps.len(),
                    pps.len()
                );
                self.stream_manager
                    .set_stream_sps_pps(local_stream_id, sps, pps);
            }
        }

        info!(
            "[RTSP Puller] [Step 3/4] Setting up {} tracks...",
            tracks_to_create.len()
        );
        for (idx, _track) in tracks_to_create.iter().enumerate() {
            let response = session.send_setup(idx).await?;
            if !response.starts_with("RTSP/1.0 200 OK") {
                return Err(anyhow::anyhow!(
                    "SETUP failed for track {}: {}",
                    idx,
                    response
                ));
            }
        }

        info!("[RTSP Puller] [Step 4/4] Sending PLAY...");
        let response = session.send_play().await?;
        if !response.starts_with("RTSP/1.0 200 OK") {
            return Err(anyhow::anyhow!("PLAY failed: {}", response));
        }

        let _ = self.stream_manager.set_publishing(local_stream_id);

        info!("[RTSP Puller] =========================================");
        info!(
            "[RTSP Puller] SUCCESS: RTSP Pull started for stream {}",
            local_stream_id
        );
        info!("[RTSP Puller] Session ID: {:?}", session.session_id());
        info!("[RTSP Puller] Remote URL: {}", remote_url);
        info!("[RTSP Puller] =========================================");

        let manager_clone = self.stream_manager.clone();
        let stream_id_clone = local_stream_id.to_string();
        let use_udp = session.use_udp();
        let udp_sockets = session.udp_track_sockets();
        let remote_url_clone = remote_url.to_string();
        let receive_tracks = tracks_to_create.clone();

        info!(
            "[RTSP Puller] Transport: {}",
            if use_udp { "UDP" } else { "TCP" }
        );

        tokio::spawn(async move {
            if use_udp {
                tokio::select! {
                    _ = Self::udp_receive_loop(udp_sockets, manager_clone.clone(), stream_id_clone.clone(), receive_tracks.clone()) => (),
                    _ = Self::send_keepalive(remote_url_clone) => (),
                }
            } else {
                tokio::select! {
                    _ = Self::rtp_receive_loop(manager_clone.clone(), stream_id_clone.clone(), receive_tracks.clone()) => (),
                    _ = Self::send_keepalive(remote_url_clone) => (),
                }
            }
        });

        Ok(())
    }

    async fn rtp_receive_loop(
        manager: Arc<StreamManager>,
        stream_id: String,
        tracks: Vec<Track>,
    ) {
        let mut rtsp_response = String::new();
        let mut frame_count: u64 = 0;
        let mut bytes_received: u64 = 0;
        let mut last_log_time = std::time::Instant::now();
        let video_codec = tracks
            .iter()
            .find(|t| t.id == 0)
            .map(|t| t.codec)
            .unwrap_or(CodecType::H264);
        let mut h264_ingest = H264RtpIngest::new(manager.clone(), stream_id.clone(), "RTSP-Pull");
        let mut hevc_fu = crate::core::HevcFuReassembler::default();

        info!(
            "[RTSP Puller] [RTP Loop] Starting RTP receive loop for stream {}",
            stream_id
        );

        loop {
            break;
        }

        h264_ingest.flush_remaining();
        let _ = manager.set_unpublished(&stream_id);

        info!(
            "[RTSP Puller] [RTP Loop] RTP receive loop ended for stream {}",
            stream_id
        );
    }

    async fn udp_receive_loop(
        tracks: Vec<(usize, Arc<UdpSocket>)>,
        manager: Arc<StreamManager>,
        stream_id: String,
        sdp_tracks: Vec<Track>,
    ) {
        info!(
            "[RTSP Puller] [UDP Loop] Starting for stream {} ({} tracks)",
            stream_id,
            tracks.len()
        );

        for (track_id, socket) in tracks {
            let manager = Arc::clone(&manager);
            let sid = stream_id.clone();
            let sdp_tracks = sdp_tracks.clone();

            tokio::spawn(async move {
                let mut buffer = vec![0u8; 65535];
                let mut frame_count: u64 = 0;
                let mut h264_ingest = if track_id == 0 {
                    Some(H264RtpIngest::new(
                        manager.clone(),
                        sid.clone(),
                        "RTSP-Pull-UDP",
                    ))
                } else {
                    None
                };

                loop {
                    match RtspCommon::receive_rtp_over_udp(&socket, &mut buffer).await {
                        Ok((len, _)) => {
                            if len < 12 || RtspCommon::is_rtcp_packet(&buffer[..len]) {
                                continue;
                            }

                            if track_id == 0 {
                                if let Some(ingest) = &mut h264_ingest {
                                    if ingest.ingest_rtp_packet(&buffer[..len]) {
                                        frame_count += 1;
                                    }
                                }
                            } else {
                                let marker = (buffer[1] & 0x80) != 0;
                                let ts = u64::from(u32::from_be_bytes(
                                    buffer[4..8].try_into().unwrap_or([0; 4]),
                                ));
                                let payload_type = buffer[1] & 0x7F;
                                let clock_rate = rtsp_clock_rate_for_track(
                                    &sdp_tracks,
                                    track_id as u8,
                                    payload_type,
                                );
                                let media =
                                    crate::server::webrtc::rtp_h264_media_payload(&buffer[..len])
                                        .map(|(p, _, _)| p)
                                        .unwrap_or(&buffer[12..len]);
                                let aac_data = match super::common::strip_mpeg4_generic_aac(media) {
                                    Some(raw) if !raw.is_empty() => raw,
                                    _ => continue,
                                };
                                let frame = MediaFrame {
                                    stream_id: sid.clone(),
                                    track_id: track_id as u8,
                                    timestamp: ts,
                                    clock_rate: Some(clock_rate),
                                    data: aac_data.into(),
                                    is_keyframe: marker,
                                    codec: CodecType::AAC,
                                    rtp_data: None,
                                };
                                manager.publish_frame(frame);
                                frame_count += 1;
                            }
                        }
                        Err(e) => {
                            error!("[RTSP Puller] [UDP Loop] track={} error: {}", track_id, e);
                            break;
                        }
                    }
                }

                if let Some(mut ingest) = h264_ingest {
                    ingest.flush_remaining();
                }
                info!(
                    "[RTSP Puller] [UDP Loop] track={} ended, frames={}",
                    track_id, frame_count
                );
            });
        }

        std::future::pending::<()>().await;
    }

    async fn send_keepalive(remote_url: String) {
        info!("[RTSP Puller] [Keepalive] Starting keepalive loop");
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        info!("[RTSP Puller] [Keepalive] Keepalive loop ended");
    }
}

#[async_trait]
impl MediaTransport for RtspPuller {
    fn protocol(&self) -> StreamProtocol {
        StreamProtocol::RTSP
    }

    fn status(&self) -> TransportStatus {
        self.status.clone()
    }

    fn peer_addr(&self) -> String {
        self.peer_addr.clone()
    }

    fn stream_id(&self) -> String {
        self.stream_id.clone()
    }

    async fn start(&mut self) -> Result<()> {
        self.status = TransportStatus::Connecting;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        self.status = TransportStatus::Closed;
        Ok(())
    }
}

#[async_trait]
impl StreamPuller for RtspPuller {
    async fn pull(&mut self) -> Result<()> {
        let url = self.remote_url.clone();
        if let Some(url) = url {
            self.status = TransportStatus::Playing;
            self.do_pull(&url, &self.stream_id).await?;
        }
        Ok(())
    }
}