use anyhow::Result;
use async_trait::async_trait;
use bytes::BytesMut;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::time::Duration;
use tracing::{debug, error, info, warn};

use super::client_session::RtspClientSession;
use super::common::RtspCommon;
use crate::core::{
    CodecType, MediaFrame, MediaTransport, PusherId, StreamManager, StreamProtocol, StreamPusher,
    Track, TransportStatus,
};

pub struct RtspPusher {
    id: PusherId,
    stream_manager: Arc<StreamManager>,
    remote_url: String,
    stream_id: String,
    tracks: Option<Vec<Track>>,
    status: RwLock<TransportStatus>,
    paused: Arc<RwLock<bool>>,
    vps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    sps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    pps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    peer_addr: String,
}

impl RtspPusher {
    pub fn new(stream_manager: Arc<StreamManager>, remote_url: &str, stream_id: &str) -> Self {
        let id = format!("pusher_rtsp_{}_{}", stream_id, uuid::Uuid::new_v4());
        Self {
            id,
            stream_manager,
            remote_url: remote_url.to_string(),
            stream_id: stream_id.to_string(),
            tracks: None,
            status: RwLock::new(TransportStatus::Idle),
            paused: Arc::new(RwLock::new(false)),
            vps_cache: Arc::new(parking_lot::RwLock::new(None)),
            sps_cache: Arc::new(parking_lot::RwLock::new(None)),
            pps_cache: Arc::new(parking_lot::RwLock::new(None)),
            peer_addr: String::new(),
        }
    }

    pub fn set_tracks(&mut self, tracks: Vec<Track>) {
        self.tracks = Some(tracks);
        info!(
            "[RTSP Pusher] Set {} tracks for stream {}",
            self.tracks.as_ref().unwrap().len(),
            self.stream_id
        );
    }

    fn set_status(&self, status: TransportStatus) {
        let mut s = self.status.write();
        info!(
            "[RTSP Pusher] Status changed: {} -> {}",
            s.as_str(),
            status.as_str()
        );
        *s = status;
    }

    fn is_paused(&self) -> bool {
        *self.paused.read()
    }

    fn set_paused(&self, paused: bool) {
        let mut p = self.paused.write();
        *p = paused;
    }

    pub async fn start(&mut self) -> Result<()> {
        info!("[RTSP Pusher] =========================================");
        info!(
            "[RTSP Pusher] Starting RTSP Pusher for stream: {}",
            self.stream_id
        );
        info!("[RTSP Pusher] Target URL: {}", self.remote_url);
        info!("[RTSP Pusher] =========================================");

        self.set_status(TransportStatus::Connecting);

        if self.tracks.is_none() {
            self.set_status(TransportStatus::Error("Tracks not set".to_string()));
            return Err(anyhow::anyhow!(
                "Tracks not set. Call set_tracks() before start()"
            ));
        }

        let tracks = self.tracks.as_ref().unwrap();
        info!("[RTSP Pusher] Number of tracks: {}", tracks.len());
        for (idx, track) in tracks.iter().enumerate() {
            info!(
                "[RTSP Pusher] Track {}: codec={:?}, payload_type={}, clock_rate={}",
                idx, track.codec, track.payload_type, track.clock_rate
            );
        }

        let mut session = RtspClientSession::new(self.stream_manager.clone(), &self.remote_url);

        info!("[RTSP Pusher] [Step 1/5] Connecting...");
        session.connect().await?;

        info!("[RTSP Pusher] [Step 2/5] Sending OPTIONS...");
        let response = session.send_options().await?;
        if !response.starts_with("RTSP/1.0 200 OK") {
            self.set_status(TransportStatus::Error("OPTIONS failed".to_string()));
            return Err(anyhow::anyhow!("OPTIONS failed: {}", response));
        }

        let vps = self.stream_manager.get_stream(&self.stream_id)
            .and_then(|s| s.vps.clone());
        let sps = self.stream_manager.get_stream(&self.stream_id)
            .and_then(|s| s.sps.clone());
        let pps = self.stream_manager.get_stream(&self.stream_id)
            .and_then(|s| s.pps.clone());
        let sdp = RtspClientSession::build_sdp(tracks, vps.as_deref(), sps.as_deref(), pps.as_deref());
        info!("[RTSP Pusher] [Step 3/5] Sending ANNOUNCE...");
        let response = session.send_announce(&sdp).await?;
        if !response.starts_with("RTSP/1.0 200 OK") {
            self.set_status(TransportStatus::Error("ANNOUNCE failed".to_string()));
            return Err(anyhow::anyhow!("ANNOUNCE failed: {}", response));
        }

        info!(
            "[RTSP Pusher] [Step 4/5] Setting up {} tracks...",
            tracks.len()
        );
        for (idx, track) in tracks.iter().enumerate() {
            let response = session.send_setup(idx).await?;
            if !response.starts_with("RTSP/1.0 200 OK") {
                self.set_status(TransportStatus::Error(format!(
                    "SETUP failed for track {}",
                    idx
                )));
                return Err(anyhow::anyhow!(
                    "SETUP failed for track {}: {}",
                    idx,
                    response
                ));
            }
            info!(
                "[RTSP Pusher] [Step 4/5] Track {} setup completed (codec={:?})",
                idx, track.codec
            );
        }

        info!("[RTSP Pusher] [Step 5/5] Sending RECORD...");
        let response = session.send_record().await?;
        if !response.starts_with("RTSP/1.0 200 OK") {
            self.set_status(TransportStatus::Error("RECORD failed".to_string()));
            return Err(anyhow::anyhow!("RECORD failed: {}", response));
        }

        self.set_status(TransportStatus::Publishing);
        info!("[RTSP Pusher] =========================================");
        info!(
            "[RTSP Pusher] SUCCESS: RTSP Pusher started for stream {}",
            self.stream_id
        );
        info!("[RTSP Pusher] Session ID: {:?}", session.session_id());
        info!("[RTSP Pusher] Remote URL: {}", self.remote_url);
        info!("[RTSP Pusher] Number of tracks: {}", tracks.len());
        info!("[RTSP Pusher] =========================================");

        let manager_clone = self.stream_manager.clone();
        let stream_id_clone = self.stream_id.clone();
        let tracks_clone = tracks.clone();
        let paused_clone = Arc::clone(&self.paused);
        let sps_cache_clone = Arc::clone(&self.sps_cache);
        let pps_cache_clone = Arc::clone(&self.pps_cache);
        let use_udp = session.use_udp();
        let udp_tracks: HashMap<usize, (Arc<tokio::net::UdpSocket>, SocketAddr)> = session
            .udp_track_transports()
            .into_iter()
            .map(|(id, sock, addr)| (id, (sock, addr)))
            .collect();

        info!(
            "[RTSP Pusher] Transport: {}",
            if use_udp { "UDP" } else { "TCP" }
        );

        tokio::spawn(async move {
            tokio::select! {
                _ = Self::rtp_send_loop(manager_clone, stream_id_clone, tracks_clone, paused_clone, sps_cache_clone, pps_cache_clone, use_udp, udp_tracks) => (),
            }
        });

        Ok(())
    }

    async fn rtp_send_loop(
        manager: Arc<StreamManager>,
        stream_id: String,
        tracks: Vec<Track>,
        paused: Arc<RwLock<bool>>,
        sps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
        pps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
        use_udp: bool,
        udp_tracks: HashMap<usize, (Arc<tokio::net::UdpSocket>, SocketAddr)>,
    ) {
        let mut buffer = BytesMut::with_capacity(8192);
        let mut frame_count: u64 = 0;
        let mut bytes_sent: u64 = 0;
        let mut last_log_time = std::time::Instant::now();
        let mut sequences: Vec<u16> = vec![0; tracks.len()];

        info!(
            "[RTSP Pusher] [RTP Loop] Starting RTP send loop for stream {}",
            stream_id
        );

        let mut reader = match manager
            .dispatch_subscribe(&stream_id, crate::core::DispatchPolicy::SequentialFromIdr)
        {
            Some(r) => r,
            None => {
                error!(
                    "[RTSP Pusher] [RTP Loop] Failed to subscribe to stream {}",
                    stream_id
                );
                return;
            }
        };

        info!("[RTSP Pusher] [RTP Loop] Waiting for media frames...");

        async fn send_rtp_packet(
            use_udp: bool,
            udp_tracks: &HashMap<usize, (Arc<tokio::net::UdpSocket>, SocketAddr)>,
            track_id: usize,
            packet: &[u8],
        ) -> Result<()> {
            if use_udp {
                if let Some((socket, addr)) = udp_tracks.get(&track_id) {
                    RtspCommon::send_rtp_over_udp(socket, packet, *addr).await?;
                }
            }
            Ok(())
        }

        loop {
            let frames = match reader.recv_batch().await {
                Ok(f) if !f.is_empty() => f,
                Ok(_) => continue,
                Err(crate::core::dispatch::DispatchError::Closed) => break,
            };
            for frame in frames {
                frame_count += 1;

                if *paused.read() {
                    debug!(
                        "[RTSP Pusher] [RTP Loop] Frame #{:>6} - Dropping frame, pusher is paused",
                        frame_count
                    );
                    continue;
                }

                let track = tracks.iter().find(|t| t.id == frame.track_id);
                if track.is_none() {
                    warn!(
                        "[RTSP Pusher] [RTP Loop] Frame #{:>6} - No track found for track_id={}",
                        frame_count, frame.track_id
                    );
                    continue;
                }

                let track = track.unwrap();

                if frame.codec == CodecType::H264 && frame.is_keyframe {
                    let (sps, pps) = RtspCommon::extract_sps_pps(&frame.data);

                    if let Some(sps_data) = sps {
                        let mut cached_sps = sps_cache.write();
                        if cached_sps.is_none() {
                            info!(
                                "[RTSP Pusher] [RTP Loop] Cached SPS: {} bytes",
                                sps_data.len()
                            );
                        }
                        *cached_sps = Some(sps_data);
                    }

                    if let Some(pps_data) = pps {
                        let mut cached_pps = pps_cache.write();
                        if cached_pps.is_none() {
                            info!(
                                "[RTSP Pusher] [RTP Loop] Cached PPS: {} bytes",
                                pps_data.len()
                            );
                        }
                        *cached_pps = Some(pps_data);
                    }

                    let sps_copy = sps_cache.read().clone();
                    let pps_copy = pps_cache.read().clone();

                    if let (Some(sps_data), Some(pps_data)) = (sps_copy, pps_copy) {
                        let timestamp = frame.timestamp as u32;
                        let seq = sequences[track.id as usize];

                        let sps_rtp = RtspClientSession::build_rtp_packet(
                            96, seq, timestamp, 0x12345678, false, &sps_data,
                        );
                        if let Err(e) = send_rtp_packet(
                            use_udp,
                            &udp_tracks,
                            track.id as usize,
                            &sps_rtp,
                        )
                        .await
                        {
                            error!("[RTSP Pusher] [RTP Loop] Failed to send SPS: {}", e);
                            break;
                        }
                        info!(
                            "[RTSP Pusher] [RTP Loop] Sent SPS RTP ({} bytes)",
                            sps_rtp.len()
                        );
                        sequences[track.id as usize] = sequences[track.id as usize].wrapping_add(1);

                        let pps_rtp = RtspClientSession::build_rtp_packet(
                            96,
                            sequences[track.id as usize],
                            timestamp,
                            0x12345678,
                            false,
                            &pps_data,
                        );
                        if let Err(e) = send_rtp_packet(
                            use_udp,
                            &udp_tracks,
                            track.id as usize,
                            &pps_rtp,
                        )
                        .await
                        {
                            error!("[RTSP Pusher] [RTP Loop] Failed to send PPS: {}", e);
                            break;
                        }
                        info!(
                            "[RTSP Pusher] [RTP Loop] Sent PPS RTP ({} bytes)",
                            pps_rtp.len()
                        );
                        sequences[track.id as usize] = sequences[track.id as usize].wrapping_add(1);
                    }
                }

                let seq = sequences[track.id as usize];

                let rtp_payload = if frame.codec == CodecType::H264 {
                    let mut payload = Vec::with_capacity(4 + frame.data.len());
                    payload.extend_from_slice(&(frame.data.len() as u32).to_be_bytes());
                    payload.extend_from_slice(&frame.data);
                    payload
                } else {
                    frame.data.to_vec()
                };

                let rtp_packet = RtspClientSession::build_rtp_packet(
                    track.payload_type,
                    seq,
                    frame.timestamp as u32,
                    0x12345678,
                    frame.is_keyframe,
                    &rtp_payload,
                );

                if use_udp {
                    bytes_sent += rtp_packet.len() as u64;
                }

                if let Err(e) = send_rtp_packet(
                    use_udp,
                    &udp_tracks,
                    track.id as usize,
                    &rtp_packet,
                )
                .await
                {
                    error!(
                        "[RTSP Pusher] [RTP Loop] Frame #{:>6} - Failed to send RTP packet: {}",
                        frame_count, e
                    );
                    break;
                }

                if !use_udp {
                    bytes_sent += (4 + rtp_packet.len()) as u64;
                }

                sequences[track.id as usize] = sequences[track.id as usize].wrapping_add(1);

                let elapsed = last_log_time.elapsed();
                if elapsed >= Duration::from_secs(10) {
                    let fps = frame_count as f64 / elapsed.as_secs_f64();
                    let bps = bytes_sent as f64 / elapsed.as_secs_f64();
                    info!("[RTSP Pusher] [RTP Loop] Stats - Frames: {}, Bytes: {}, FPS: {:.2}, BPS: {:.2} KB/s", 
                      frame_count, bytes_sent, fps, bps / 1024.0);
                    last_log_time = std::time::Instant::now();
                }

                buffer.clear();
            }
        }

        info!(
            "[RTSP Pusher] [RTP Loop] RTP send loop ended for stream {}",
            stream_id
        );
    }

    pub fn is_running(&self) -> bool {
        matches!(*self.status.read(), TransportStatus::Publishing)
    }
}

#[async_trait]
impl MediaTransport for RtspPusher {
    fn protocol(&self) -> StreamProtocol {
        StreamProtocol::RTSP
    }

    fn status(&self) -> TransportStatus {
        self.status.read().clone()
    }

    fn peer_addr(&self) -> String {
        self.peer_addr.clone()
    }

    fn stream_id(&self) -> String {
        self.stream_id.clone()
    }

    async fn start(&mut self) -> Result<()> {
        Self::start(self).await
    }

    async fn stop(&mut self) -> Result<()> {
        info!(
            "[RTSP Pusher] Stopping pusher for stream {}",
            self.stream_id
        );
        self.set_status(TransportStatus::Closed);
        Ok(())
    }
}

#[async_trait]
impl StreamPusher for RtspPusher {
    fn id(&self) -> &PusherId {
        &self.id
    }

    fn remote_url(&self) -> &str {
        &self.remote_url
    }

    async fn pause(&mut self) -> Result<()> {
        info!("[RTSP Pusher] Pausing pusher for stream {}", self.stream_id);
        self.set_paused(true);
        let current_status = self.status.read().clone();
        if matches!(current_status, TransportStatus::Publishing) {
            self.set_status(TransportStatus::Paused);
        }
        Ok(())
    }

    async fn resume(&mut self) -> Result<()> {
        info!(
            "[RTSP Pusher] Resuming pusher for stream {}",
            self.stream_id
        );
        self.set_paused(false);
        let current_status = self.status.read().clone();
        if current_status == TransportStatus::Paused {
            self.set_status(TransportStatus::Publishing);
        }
        Ok(())
    }
}