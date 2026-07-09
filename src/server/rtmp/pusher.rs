use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bytes::BytesMut;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, error, info, warn};

use super::amf0::{self, Amf0Value};
use super::chunk::{self, ChunkAssembler, ChunkHeader, RtmpMessage};
use super::connection::RtmpConnection;
use super::puller::{parse_rtmp_url, RtmpUrl};
use super::session::{
    build_aac_sequence_header, build_avc_sequence_header, build_hevc_sequence_header,
    build_metadata, frame_to_rtmp_audio, frame_to_rtmp_video,
};
use crate::core::{
    iter_annex_b_nal_ranges, CodecType, MediaFrame, MediaTransport, PusherId, StreamManager,
    StreamProtocol, StreamPusher, Track, TransportStatus,
};

pub struct RtmpPusher {
    id: PusherId,
    stream_manager: Arc<StreamManager>,
    remote_url: String,
    stream_id: String,
    tracks: Option<Vec<Track>>,
    status: RwLock<TransportStatus>,
    paused: Arc<RwLock<bool>>,
    sps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    pps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    vps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
    peer_addr: String,
}

impl RtmpPusher {
    pub fn new(stream_manager: Arc<StreamManager>, remote_url: &str, stream_id: &str) -> Self {
        let id = format!("pusher_rtmp_{}_{}", stream_id, uuid::Uuid::new_v4());
        Self {
            id,
            stream_manager,
            remote_url: remote_url.to_string(),
            stream_id: stream_id.to_string(),
            tracks: None,
            status: RwLock::new(TransportStatus::Idle),
            paused: Arc::new(RwLock::new(false)),
            sps_cache: Arc::new(parking_lot::RwLock::new(None)),
            pps_cache: Arc::new(parking_lot::RwLock::new(None)),
            vps_cache: Arc::new(parking_lot::RwLock::new(None)),
            peer_addr: String::new(),
        }
    }

    pub fn set_tracks(&mut self, tracks: Vec<Track>) {
        self.tracks = Some(tracks);
        info!(
            "[RTMP Pusher] Set {} tracks for stream {}",
            self.tracks.as_ref().unwrap().len(),
            self.stream_id
        );
    }

    fn set_status(&self, status: TransportStatus) {
        let mut s = self.status.write();
        info!(
            "[RTMP Pusher] Status changed: {} -> {}",
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

    async fn rtmp_send_loop(
        mut writer: tokio::net::tcp::OwnedWriteHalf,
        manager: Arc<StreamManager>,
        stream_id: String,
        tracks: Vec<Track>,
        paused: Arc<RwLock<bool>>,
        sps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
        pps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
        vps_cache: Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
        msg_stream_id: u32,
        chunk_size: usize,
    ) {
        let mut frame_count: u64 = 0;
        let mut bytes_sent: u64 = 0;
        let mut last_log_time = std::time::Instant::now();

        info!(
            "[RTMP Pusher] [Send Loop] Starting RTMP send loop for stream {}",
            stream_id
        );

        let mut reader = match manager
            .dispatch_subscribe(&stream_id, crate::core::DispatchPolicy::SequentialFromIdr)
        {
            Some(r) => r,
            None => {
                error!(
                    "[RTMP Pusher] [Send Loop] Failed to subscribe to stream {}",
                    stream_id
                );
                return;
            }
        };

        info!("[RTMP Pusher] [Send Loop] Waiting for media frames...");

        let mut sent_sequence_headers = false;

        loop {
            let frames = match reader.recv_batch().await {
                Ok(f) if !f.is_empty() => f,
                Ok(_) => continue,
                Err(crate::core::dispatch::DispatchError::Closed) => break,
                Err(e) => {
                    error!("[RTMP Pusher] [Send Loop] Receive error: {:?}", e);
                    break;
                }
            };

            for frame in frames {
                frame_count += 1;

                if *paused.read() {
                    debug!(
                        "[RTMP Pusher] [Send Loop] Frame #{:>6} - Dropping frame, pusher is paused",
                        frame_count
                    );
                    continue;
                }

                let track = tracks.iter().find(|t| t.id == frame.track_id);
                if track.is_none() {
                    warn!(
                        "[RTMP Pusher] [Send Loop] Frame #{:>6} - No track found for track_id={}",
                        frame_count, frame.track_id
                    );
                    continue;
                }

                let track = track.unwrap();

                if !sent_sequence_headers && frame_count == 1 {
                    Self::send_sequence_headers(&mut writer, &tracks, msg_stream_id, chunk_size)
                        .await;
                    sent_sequence_headers = true;
                }

                match track.codec {
                    CodecType::H264 => {
                        Self::cache_and_send_h264_frame(
                            &mut writer,
                            &frame,
                            &sps_cache,
                            &pps_cache,
                            msg_stream_id,
                            chunk_size,
                            frame_count,
                        )
                        .await;
                    }
                    CodecType::H265 => {
                        Self::cache_and_send_h265_frame(
                            &mut writer,
                            &frame,
                            &vps_cache,
                            &sps_cache,
                            &pps_cache,
                            msg_stream_id,
                            chunk_size,
                            frame_count,
                        )
                        .await;
                    }
                    CodecType::AAC => {
                        let rtmp_data = frame_to_rtmp_audio(&frame);
                        Self::send_rtmp_message(
                            &mut writer,
                            0x08,
                            msg_stream_id,
                            frame.timestamp as u32,
                            &rtmp_data,
                            chunk_size,
                            frame_count,
                        )
                        .await;
                        bytes_sent += rtmp_data.len() as u64;
                    }
                    _ => {
                        warn!(
                            "[RTMP Pusher] [Send Loop] Frame #{:>6} - Unsupported codec: {:?}",
                            frame_count, track.codec
                        );
                    }
                }

                if last_log_time.elapsed().as_secs() >= 30 {
                    info!(
                        "[RTMP Pusher] [Send Loop] Stream {} - Frames sent: {}, Bytes sent: {}",
                        stream_id, frame_count, bytes_sent
                    );
                    last_log_time = std::time::Instant::now();
                }
            }
        }

        info!(
            "[RTMP Pusher] [Send Loop] Send loop ended for stream '{}', total frames={}, bytes={}",
            stream_id, frame_count, bytes_sent
        );
    }

    async fn send_sequence_headers(
        writer: &mut tokio::net::tcp::OwnedWriteHalf,
        tracks: &[Track],
        msg_stream_id: u32,
        chunk_size: usize,
    ) {
        let mut video_track: Option<&Track> = None;
        let mut audio_track: Option<&Track> = None;

        for track in tracks {
            match track.codec {
                CodecType::H264 | CodecType::H265 => {
                    video_track = Some(track);
                }
                CodecType::AAC => {
                    audio_track = Some(track);
                }
                _ => {}
            }
        }

        if let Some(video_track) = video_track {
            match video_track.codec {
                CodecType::H264 => {
                    let sps_data = vec![
                        0x67, 0x42, 0x00, 0x1F, 0xD9, 0x01, 0x40, 0x1E, 0x16, 0xDD, 0x00,
                    ];
                    let pps_data = vec![0x68, 0xCE, 0x38, 0x80];
                    let seq_header = build_avc_sequence_header(&sps_data, &pps_data);
                    let _ = Self::send_rtmp_message(
                        writer,
                        0x09,
                        msg_stream_id,
                        0,
                        &seq_header,
                        chunk_size,
                        0,
                    )
                    .await;
                }
                CodecType::H265 => {
                    let vps_data = vec![0x40, 0x01, 0x0C, 0x01];
                    let sps_data = vec![0x42, 0x01, 0x01, 0x60];
                    let pps_data = vec![0x44, 0x01, 0xC0, 0x73];
                    let seq_header = build_hevc_sequence_header(&vps_data, &sps_data, &pps_data);
                    let _ = Self::send_rtmp_message(
                        writer,
                        0x09,
                        msg_stream_id,
                        0,
                        &seq_header,
                        chunk_size,
                        0,
                    )
                    .await;
                }
                _ => {}
            }
        }

        if audio_track.is_some() {
            let seq_header = build_aac_sequence_header();
            let _ =
                Self::send_rtmp_message(writer, 0x08, msg_stream_id, 0, &seq_header, chunk_size, 0)
                    .await;
        }

        let metadata = build_metadata(1920, 1080, "");
        let _ =
            Self::send_rtmp_message(writer, 0x12, msg_stream_id, 0, &metadata, chunk_size, 0).await;
    }

    async fn cache_and_send_h264_frame(
        writer: &mut tokio::net::tcp::OwnedWriteHalf,
        frame: &MediaFrame,
        sps_cache: &Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
        pps_cache: &Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
        msg_stream_id: u32,
        chunk_size: usize,
        frame_count: u64,
    ) {
        if frame.is_keyframe {
            let ranges = iter_annex_b_nal_ranges(&frame.data);
            for (start, end) in ranges {
                let nalu = &frame.data[start..end];
                if nalu.is_empty() {
                    continue;
                }
                let nal_type = nalu[0] & 0x1F;
                match nal_type {
                    7 => {
                        let mut cached_sps = sps_cache.write();
                        *cached_sps = Some(nalu.to_vec());
                    }
                    8 => {
                        let mut cached_pps = pps_cache.write();
                        *cached_pps = Some(nalu.to_vec());
                    }
                    _ => {}
                }
            }
        }

        let rtmp_data = frame_to_rtmp_video(frame);
        let _ = Self::send_rtmp_message(
            writer,
            0x09,
            msg_stream_id,
            frame.timestamp as u32,
            &rtmp_data,
            chunk_size,
            frame_count,
        )
        .await;
    }

    async fn cache_and_send_h265_frame(
        writer: &mut tokio::net::tcp::OwnedWriteHalf,
        frame: &MediaFrame,
        vps_cache: &Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
        sps_cache: &Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
        pps_cache: &Arc<parking_lot::RwLock<Option<Vec<u8>>>>,
        msg_stream_id: u32,
        chunk_size: usize,
        frame_count: u64,
    ) {
        if frame.is_keyframe {
            let ranges = iter_annex_b_nal_ranges(&frame.data);
            for (start, end) in ranges {
                let nalu = &frame.data[start..end];
                if nalu.is_empty() {
                    continue;
                }
                let nal_type = (nalu[0] >> 1) & 0x3F;
                match nal_type {
                    32 => {
                        let mut cached_vps = vps_cache.write();
                        *cached_vps = Some(nalu.to_vec());
                    }
                    33 => {
                        let mut cached_sps = sps_cache.write();
                        *cached_sps = Some(nalu.to_vec());
                    }
                    34 => {
                        let mut cached_pps = pps_cache.write();
                        *cached_pps = Some(nalu.to_vec());
                    }
                    _ => {}
                }
            }
        }

        let rtmp_data = frame_to_rtmp_video(frame);
        let _ = Self::send_rtmp_message(
            writer,
            0x09,
            msg_stream_id,
            frame.timestamp as u32,
            &rtmp_data,
            chunk_size,
            frame_count,
        )
        .await;
    }

    async fn send_rtmp_message(
        writer: &mut tokio::net::tcp::OwnedWriteHalf,
        msg_type: u8,
        msg_stream_id: u32,
        timestamp: u32,
        payload: &[u8],
        chunk_size: usize,
        frame_count: u64,
    ) -> Result<()> {
        let message =
            chunk::encode_message(msg_type, timestamp, msg_stream_id, payload, chunk_size, 3);
        if let Err(e) = writer.write_all(&message).await {
            error!(
                "[RTMP Pusher] [Send Loop] Frame #{:>6} - Write error: {}",
                frame_count, e
            );
            return Err(e.into());
        }
        if let Err(e) = writer.flush().await {
            error!(
                "[RTMP Pusher] [Send Loop] Frame #{:>6} - Flush error: {}",
                frame_count, e
            );
            return Err(e.into());
        }
        Ok(())
    }

    async fn client_handshake(conn: &mut RtmpConnection) -> Result<()> {
        conn.client_handshake().await
    }
}

#[async_trait]
impl MediaTransport for RtmpPusher {
    fn protocol(&self) -> StreamProtocol {
        StreamProtocol::RTMP
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
        info!("[RTMP Pusher] =========================================");
        info!(
            "[RTMP Pusher] Starting RTMP Pusher for stream: {}",
            self.stream_id
        );
        info!("[RTMP Pusher] Target URL: {}", self.remote_url);
        info!("[RTMP Pusher] =========================================");

        self.set_status(TransportStatus::Connecting);

        if self.tracks.is_none() {
            self.set_status(TransportStatus::Error("Tracks not set".to_string()));
            return Err(anyhow!("Tracks not set. Call set_tracks() before start()"));
        }

        let tracks = self.tracks.as_ref().unwrap();
        info!("[RTMP Pusher] Number of tracks: {}", tracks.len());
        for (idx, track) in tracks.iter().enumerate() {
            info!(
                "[RTMP Pusher] Track {}: codec={:?}, payload_type={}, clock_rate={}",
                idx, track.codec, track.payload_type, track.clock_rate
            );
        }

        let rtmp_url = parse_rtmp_url(&self.remote_url)?;
        let addr = format!("{}:{}", rtmp_url.host, rtmp_url.port);

        info!("[RTMP Pusher] [Step 1/5] Connecting to {}...", addr);
        let stream = TcpStream::connect(&addr).await?;
        self.peer_addr = addr.clone();

        info!("[RTMP Pusher] [Step 2/5] Performing handshake...");
        let mut conn = RtmpConnection::new(stream);
        Self::client_handshake(&mut conn).await?;

        let mut tx_id = 1.0_f64;
        let mut server_stream_id: u32 = 1;

        info!("[RTMP Pusher] [Step 3/5] Sending connect...");
        let connect_msg =
            super::puller::build_connect_command(tx_id, &rtmp_url.app, &rtmp_url.tc_url);
        conn.send_message(0x14, 0, &connect_msg).await?;

        let _ = wait_for_result(&mut conn, tx_id, "connect").await?;
        tx_id += 1.0;

        info!("[RTMP Pusher] [Step 4/5] Sending createStream...");
        let create_msg = super::puller::build_create_stream_command(tx_id);
        conn.send_message(0x14, 0, &create_msg).await?;

        server_stream_id = wait_for_create_stream(&mut conn, tx_id).await? as u32;
        tx_id += 1.0;

        info!("[RTMP Pusher] [Step 5/5] Sending publish...");
        let publish_msg = build_publish_command(tx_id, &rtmp_url.stream_name, server_stream_id);
        conn.send_message(0x14, server_stream_id, &publish_msg)
            .await?;

        let _ = wait_for_publish_start(&mut conn).await?;

        self.set_status(TransportStatus::Publishing);
        info!("[RTMP Pusher] =========================================");
        info!(
            "[RTMP Pusher] SUCCESS: RTMP Pusher started for stream {}",
            self.stream_id
        );
        info!("[RTMP Pusher] Session ID: {}", server_stream_id);
        info!("[RTMP Pusher] Remote URL: {}", self.remote_url);
        info!("[RTMP Pusher] Number of tracks: {}", tracks.len());
        info!("[RTMP Pusher] =========================================");

        let manager_clone = self.stream_manager.clone();
        let stream_id_clone = self.stream_id.clone();
        let tracks_clone = tracks.clone();
        let paused_clone = Arc::clone(&self.paused);
        let sps_cache_clone = Arc::clone(&self.sps_cache);
        let pps_cache_clone = Arc::clone(&self.pps_cache);
        let vps_cache_clone = Arc::clone(&self.vps_cache);
        let chunk_size = conn.chunk_size();

        let (reader, writer) = conn.into_inner();

        tokio::spawn(async move {
            tokio::select! {
                _ = Self::rtmp_send_loop(writer, manager_clone, stream_id_clone, tracks_clone, paused_clone, sps_cache_clone, pps_cache_clone, vps_cache_clone, server_stream_id, chunk_size) => (),
                _ = monitor_connection(reader) => (),
            }
        });

        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        if matches!(
            self.status(),
            TransportStatus::Closed | TransportStatus::Error(_)
        ) {
            return Ok(());
        }
        self.set_status(TransportStatus::Closed);
        info!("[RTMP Pusher] Stopped pusher for stream {}", self.stream_id);
        Ok(())
    }
}

#[async_trait]
impl StreamPusher for RtmpPusher {
    fn id(&self) -> &PusherId {
        &self.id
    }

    fn remote_url(&self) -> &str {
        &self.remote_url
    }

    async fn pause(&mut self) -> Result<()> {
        if matches!(
            self.status(),
            TransportStatus::Closed | TransportStatus::Error(_)
        ) {
            return Err(anyhow!("Pusher is not running"));
        }
        self.set_paused(true);
        self.set_status(TransportStatus::Paused);
        info!("[RTMP Pusher] Paused pusher for stream {}", self.stream_id);
        Ok(())
    }

    async fn resume(&mut self) -> Result<()> {
        if self.status() != TransportStatus::Paused {
            return Err(anyhow!("Pusher is not paused"));
        }
        self.set_paused(false);
        self.set_status(TransportStatus::Publishing);
        info!("[RTMP Pusher] Resumed pusher for stream {}", self.stream_id);
        Ok(())
    }
}

fn build_publish_command(tx_id: f64, stream_name: &str, msg_stream_id: u32) -> Vec<u8> {
    let payload = amf0::encode(&[
        Amf0Value::String("publish".to_string()),
        Amf0Value::Number(tx_id),
        Amf0Value::Null,
        Amf0Value::String(stream_name.to_string()),
        Amf0Value::String("live".to_string()),
    ]);
    chunk::encode_message(0x14, 0, msg_stream_id, &payload, 128, 3)
}

async fn wait_for_result(conn: &mut RtmpConnection, tx_id: f64, command: &str) -> Result<()> {
    for _ in 0..200 {
        for msg in conn.parse_chunks() {
            if let Some(new_size) = conn.handle_control_message(&msg.payload, msg.msg_type) {
                conn.set_chunk_size(new_size);
            }

            if msg.msg_type == 0x14 {
                if let Ok((values, _)) = amf0::decode(&msg.payload) {
                    if let Some(Amf0Value::String(cmd)) = values.get(0) {
                        if cmd == "_result" || cmd == "_error" {
                            if let Some(Amf0Value::Number(id)) = values.get(1) {
                                if (id - tx_id).abs() < 0.001 {
                                    info!(
                                        "[RTMP Pusher] Received {} response for {}",
                                        cmd, command
                                    );
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
        }

        let len = conn.read_timeout(Duration::from_millis(100)).await?;
        if len == 0 {
            return Err(anyhow!("Remote connection closed"));
        }
    }

    Err(anyhow!("Timeout waiting for {} response", command))
}

async fn wait_for_create_stream(conn: &mut RtmpConnection, tx_id: f64) -> Result<f64> {
    for _ in 0..200 {
        for msg in conn.parse_chunks() {
            if let Some(new_size) = conn.handle_control_message(&msg.payload, msg.msg_type) {
                conn.set_chunk_size(new_size);
            }

            if msg.msg_type == 0x14 {
                if let Ok((values, _)) = amf0::decode(&msg.payload) {
                    if let Some(Amf0Value::String(cmd)) = values.get(0) {
                        if cmd == "_result" {
                            if let Some(Amf0Value::Number(id)) = values.get(1) {
                                if (id - tx_id).abs() < 0.001 {
                                    if let Some(Amf0Value::Number(stream_id)) = values.get(3) {
                                        info!("[RTMP Pusher] Received createStream response, stream_id={}", stream_id);
                                        return Ok(*stream_id);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let len = conn.read_timeout(Duration::from_millis(100)).await?;
        if len == 0 {
            return Err(anyhow!("Remote connection closed"));
        }
    }

    Err(anyhow!("Timeout waiting for createStream response"))
}

async fn wait_for_publish_start(conn: &mut RtmpConnection) -> Result<()> {
    for _ in 0..200 {
        let mut publish_started = false;

        for msg in conn.parse_chunks() {
            if let Some(new_size) = conn.handle_control_message(&msg.payload, msg.msg_type) {
                conn.set_chunk_size(new_size);
            }

            if msg.msg_type == 0x14 {
                if let Ok((values, _)) = amf0::decode(&msg.payload) {
                    for val in values {
                        if let Amf0Value::String(s) = val {
                            if s == "onStatus" {
                                publish_started = true;
                                break;
                            }
                        }
                    }
                }
            }
        }

        if publish_started {
            return Ok(());
        }

        let len = conn.read_timeout(Duration::from_millis(100)).await?;
        if len == 0 {
            return Err(anyhow!("Remote connection closed"));
        }
    }

    Err(anyhow!("Timeout waiting for publish start"))
}

async fn monitor_connection(mut reader: tokio::net::tcp::OwnedReadHalf) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => {
                info!("[RTMP Pusher] Remote connection closed");
                break;
            }
            Ok(_) => {}
            Err(e) => {
                error!("[RTMP Pusher] Connection error: {}", e);
                break;
            }
        }
    }
}

use std::time::Duration;
