/// HLS (HTTP Live Streaming) module
/// Subscribes to streams and generates HLS segments (MPEG-TS) with M3U8 playlists.
pub mod m3u8;
pub mod ts_muxer;

use std::collections::HashMap;
use std::sync::Arc;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use parking_lot::RwLock;
use tokio::fs;
use tracing::{info, warn, error, debug};
use anyhow::Result;

use crate::core::{StreamManager, MediaFrame, CodecType, drain_broadcast_lag};
use crate::webrtc::{annex_b_with_config, request_publisher_keyframe, h264_util::is_keyframe_annex_b};
use self::m3u8::M3u8Generator;
use self::ts_muxer::TsMuxer;

/// PAT/PMT only; segments smaller than this are not committed.
const MIN_SEGMENT_BYTES: usize = 512;

fn is_hls_video_keyframe(frame: &MediaFrame) -> bool {
    matches!(frame.codec, CodecType::H264 | CodecType::H265)
        && (frame.is_keyframe || is_keyframe_annex_b(&frame.data))
}

/// HLS configuration
#[derive(Debug, Clone)]
pub struct HlsConfig {
    pub enabled: bool,
    pub segment_duration: f64,
    pub max_segments: usize,
    pub output_dir: String,
}

impl Default for HlsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            segment_duration: 2.0,
            max_segments: 10,
            output_dir: "./hls".to_string(),
        }
    }
}

/// Per-stream HLS session
struct HlsSession {
    stream_id: String,
    muxer: TsMuxer,
    playlist: M3u8Generator,
    /// Current segment buffer
    segment_buffer: Vec<u8>,
    /// Current segment duration accumulator (from video timestamps)
    segment_duration_acc: f64,
    /// Output directory for this stream
    output_dir: PathBuf,
    /// Whether this session is active
    active: bool,
    /// Drop video until the first keyframe (late HLS join)
    primed: bool,
    /// Session-wide video mux timeline (ms), never reset at segment splits
    session_video_mux_ms: u64,
    /// Last raw RTMP video timestamp used for delta accumulation
    last_raw_video_ts: Option<u64>,
    /// Session-wide AAC frame count for continuous audio PTS
    session_audio_frames: u64,
    /// Last raw video timestamp for segment duration (ms)
    last_video_timestamp: u64,
}

/// A completed TS segment ready to write to disk.
struct CompletedSegment {
    data: Vec<u8>,
    filename: String,
    duration: f64,
    seq: u64,
}

impl HlsSession {
    fn new(stream_id: &str, config: &HlsConfig) -> Result<Self> {
        let output_dir = PathBuf::from(&config.output_dir).join(stream_id);

        // Remove stale segments from a previous server run
        if output_dir.exists() {
            let _ = std::fs::remove_dir_all(&output_dir);
        }
        std::fs::create_dir_all(&output_dir)?;

        Ok(Self {
            stream_id: stream_id.to_string(),
            muxer: TsMuxer::new(),
            playlist: M3u8Generator::new(config.segment_duration, config.max_segments),
            segment_buffer: Vec::new(),
            segment_duration_acc: 0.0,
            output_dir,
            active: true,
            primed: false,
            session_video_mux_ms: 0,
            last_raw_video_ts: None,
            session_audio_frames: 0,
            last_video_timestamp: 0,
        })
    }

    /// Discard partial segment after falling behind; wait for next IDR to re-prime.
    fn recover_from_lag(&mut self) {
        self.segment_buffer.clear();
        self.segment_duration_acc = 0.0;
        self.last_video_timestamp = 0;
        self.primed = false;
        self.begin_new_segment();
    }

    /// Reset muxer and per-segment bookkeeping for a new TS file.
    fn begin_new_segment(&mut self) {
        self.segment_duration_acc = 0.0;
        self.muxer.reset_for_new_segment();
    }

    fn mux_timestamp_ms(&mut self, frame: &MediaFrame) -> u64 {
        match frame.codec {
            CodecType::AAC => {
                let pts = self.session_audio_frames * 1024 * 1000 / 44100;
                self.session_audio_frames += 1;
                pts
            }
            CodecType::H264 | CodecType::H265 => {
                if let Some(last) = self.last_raw_video_ts {
                    if frame.timestamp > last {
                        let delta_ms = crate::core::media_timestamp_delta_ms(last, frame.timestamp);
                        if delta_ms > 0 && delta_ms < 2000 {
                            self.session_video_mux_ms += delta_ms;
                        }
                    }
                }
                self.last_raw_video_ts = Some(frame.timestamp);
                self.session_video_mux_ms
            }
            _ => frame.timestamp,
        }
    }

    fn prepare_frame_for_mux(&mut self, frame: &MediaFrame) -> MediaFrame {
        let ts = self.mux_timestamp_ms(frame);
        MediaFrame::new(
            frame.stream_id.clone(),
            frame.track_id,
            ts,
            frame.data.clone(),
            frame.is_keyframe,
            frame.codec,
        )
    }

    /// Process an incoming media frame; returns a completed segment when splitting.
    fn on_frame(&mut self, frame: &MediaFrame) -> Result<Option<CompletedSegment>> {
        if !self.active {
            return Ok(None);
        }

        // HLS muxer only supports AAC audio; WebRTC publishes Opus.
        if matches!(frame.codec, CodecType::Opus | CodecType::G711) {
            return Ok(None);
        }

        // Skip AAC sequence header / tiny config payloads
        if frame.codec == CodecType::AAC && frame.data.len() < 8 {
            return Ok(None);
        }

        if !self.primed {
            if !is_hls_video_keyframe(frame) {
                return Ok(None);
            }
            self.primed = true;
            info!("[HLS] [{}] Primed on video keyframe ({} bytes)", self.stream_id, frame.data.len());
        }

        // Initialize segment with PAT/PMT
        if self.segment_buffer.is_empty() {
            let has_audio =
                self.session_audio_frames > 0 || frame.codec == CodecType::AAC;
            let pat_pmt = self.muxer.generate_pat_pmt(true, has_audio);
            self.segment_buffer.extend(pat_pmt);
        }

        // Track segment duration from video timestamps only (audio/video RTMP clocks differ)
        if matches!(frame.codec, CodecType::H264 | CodecType::H265) {
            if self.last_video_timestamp > 0 && frame.timestamp > self.last_video_timestamp {
                let delta_ms =
                    crate::core::media_timestamp_delta_ms(self.last_video_timestamp, frame.timestamp);
                if delta_ms > 0 && delta_ms < 2000 {
                    self.segment_duration_acc += delta_ms as f64 / 1000.0;
                }
            }
            self.last_video_timestamp = frame.timestamp;
        }

        // First segment uses a shorter threshold for faster startup
        let split_threshold = if self.playlist.segment_count() == 0 {
            (self.playlist.target_duration() * 0.5).clamp(1.0, 2.0)
        } else {
            self.playlist.target_duration()
        };

        let should_split = is_hls_video_keyframe(frame)
            && (self.segment_duration_acc >= split_threshold
                || (self.playlist.segment_count() == 0
                    && self.segment_buffer.len() > 8 * 1024));

        let mut completed: Option<CompletedSegment> = None;

        if should_split && self.segment_buffer.len() > MIN_SEGMENT_BYTES {
            let seq = self.playlist.next_sequence();
            let filename = M3u8Generator::slot_filename(seq, self.playlist.max_segments());
            let duration = self.segment_duration_acc.max(0.1);
            let data = std::mem::take(&mut self.segment_buffer);

            completed = Some(CompletedSegment {
                data,
                filename,
                duration,
                seq,
            });

            // Start new segment (continuous timestamps, fresh TS continuity counters)
            self.begin_new_segment();
            let has_audio = self.session_audio_frames > 0;
            let pat_pmt = self.muxer.generate_pat_pmt(true, has_audio);
            self.segment_buffer.extend(pat_pmt);
        }

        let mux_frame = self.prepare_frame_for_mux(frame);
        self.muxer.update_pcr(mux_frame.timestamp);
        let ts_data = self.muxer.frame_to_ts(&mux_frame);
        self.segment_buffer.extend(ts_data);

        Ok(completed)
    }

    /// Get the M3U8 playlist content
    pub fn get_playlist(&self) -> String {
        self.playlist.generate()
    }

    /// Get the output directory
    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }
}

/// Stop HLS generation when no playlist/segment requests arrive for this long.
const HLS_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct HlsServer {
    stream_manager: Arc<StreamManager>,
    config: HlsConfig,
    sessions: Arc<RwLock<HashMap<String, Arc<RwLock<HlsSession>>>>>,
    task_aborts: Arc<RwLock<HashMap<String, tokio::task::AbortHandle>>>,
    last_access: Arc<RwLock<HashMap<String, Instant>>>,
}

impl HlsServer {
    pub fn new(stream_manager: Arc<StreamManager>, config: HlsConfig) -> Self {
        Self {
            stream_manager,
            config,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            task_aborts: Arc::new(RwLock::new(HashMap::new())),
            last_access: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn touch(&self, stream_id: &str) {
        self.last_access
            .write()
            .insert(stream_id.to_string(), Instant::now());
    }

    /// Periodically stop HLS sessions with no recent playlist/segment requests.
    pub fn start_idle_reaper(self: &Arc<Self>) {
        let server = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let now = Instant::now();
                let stale: Vec<String> = {
                    let access = server.last_access.read();
                    access
                        .iter()
                        .filter(|(_, t)| now.duration_since(**t) > HLS_IDLE_TIMEOUT)
                        .map(|(id, _)| id.clone())
                        .collect()
                };
                for stream_id in stale {
                    if server.has_stream(&stream_id) {
                        info!(
                            "[HLS] [{}] No viewers for {:?} — stopping HLS generation",
                            stream_id, HLS_IDLE_TIMEOUT
                        );
                        let _ = server.stop_stream(&stream_id).await;
                    }
                    server.last_access.write().remove(&stream_id);
                }
            }
        });
    }

    /// Ensure HLS generation is running; `reset` clears prior session and files.
    pub async fn ensure_stream(&self, stream_id: &str, reset: bool) -> Result<bool> {
        if self.stream_manager.get_stream(&stream_id.to_string()).is_none() {
            return Ok(false);
        }
        if reset && self.has_stream(stream_id) {
            self.stop_stream(stream_id).await?;
        }
        if self.has_stream(stream_id) {
            self.touch(stream_id);
            return Ok(true);
        }
        self.start_stream(stream_id).await?;
        self.touch(stream_id);
        Ok(true)
    }

    /// Restart HLS from a clean slate (e.g. new RTMP publish).
    pub async fn restart_stream(&self, stream_id: &str) -> Result<()> {
        self.ensure_stream(stream_id, true).await?;
        Ok(())
    }

    /// Empty live playlist while waiting for the first segment.
    pub fn empty_playlist(&self) -> String {
        let target = self.config.segment_duration.ceil() as u64;
        format!(
            "#EXTM3U\r\n#EXT-X-VERSION:3\r\n#EXT-X-TARGETDURATION:{target}\r\n#EXT-X-MEDIA-SEQUENCE:0\r\n"
        )
    }

    /// Start HLS generation for a stream
    pub async fn start_stream(&self, stream_id: &str) -> Result<()> {
        if self.has_stream(stream_id) {
            return Ok(());
        }

        info!("[HLS] Starting HLS generation for stream: {}", stream_id);

        let session = HlsSession::new(stream_id, &self.config)?;
        let session = Arc::new(RwLock::new(session));

        {
            let mut sessions = self.sessions.write();
            sessions.insert(stream_id.to_string(), session.clone());
        }

        // Subscribe to stream frames
        self.stream_manager.ensure_stream_broadcast(stream_id);
        let rx = match self.stream_manager.subscribe(&stream_id.to_string()) {
            Some(rx) => rx,
            None => {
                warn!("[HLS] No broadcast channel found for stream {}", stream_id);
                return Ok(());
            }
        };

        let stream_id_owned = stream_id.to_string();
        let sessions_clone = self.sessions.clone();
        let stream_manager = self.stream_manager.clone();
        let task_aborts = self.task_aborts.clone();

        let handle = tokio::spawn(async move {
            info!("[HLS] [{}] HLS frame processing loop started", stream_id_owned);
            let mut frame_count: u64 = 0;

            let mut rx = rx;
            let dropped = drain_broadcast_lag(&mut rx);
            if dropped > 0 {
                info!(
                    "[HLS] [{}] Flushed {} stale frames before live edge",
                    stream_id_owned, dropped
                );
            }
            request_publisher_keyframe(&stream_id_owned);

            loop {
                match rx.recv().await {
                    Ok(mut frame) => {
                        if matches!(frame.codec, CodecType::Opus | CodecType::G711) {
                            continue;
                        }
                        if frame.codec == CodecType::H264 && is_hls_video_keyframe(&frame) {
                            if let Some(stream) = stream_manager.get_stream(&stream_id_owned) {
                                if let (Some(sps), Some(pps)) = (&stream.sps, &stream.pps) {
                                    let data = annex_b_with_config(sps, pps, &frame.data);
                                    frame = MediaFrame::new(
                                        frame.stream_id,
                                        frame.track_id,
                                        frame.timestamp,
                                        bytes::Bytes::from(data),
                                        frame.is_keyframe,
                                        frame.codec,
                                    );
                                }
                            }
                        }

                        frame_count += 1;
                        
                        // Process frame synchronously (no await while holding lock)
                        let segment_to_write = {
                            let session_guard = {
                                let sessions = sessions_clone.read();
                                sessions.get(&stream_id_owned).cloned()
                            };

                            if let Some(session) = session_guard {
                                let mut sess = session.write();
                                match sess.on_frame(&frame) {
                                    Ok(Some(seg)) => Some((
                                        seg.data,
                                        seg.filename,
                                        seg.duration,
                                        seg.seq,
                                        sess.output_dir.clone(),
                                    )),
                                    Ok(None) => None,
                                    Err(e) => {
                                        error!("[HLS] [{}] Frame error: {}", stream_id_owned, e);
                                        None
                                    }
                                }
                            } else {
                                info!("[HLS] [{}] Session removed, stopping", stream_id_owned);
                                break;
                            }
                        };

                        // Write segment to disk outside of lock
                        if let Some((data, filename, duration, seq, output_dir)) = segment_to_write {
                            let filepath = output_dir.join(&filename);
                            let tmp_path = output_dir.join(format!("{}.part", filename));
                            let write_ok = if let Err(e) = fs::write(&tmp_path, &data).await {
                                error!("[HLS] [{}] Failed to write segment: {}", stream_id_owned, e);
                                false
                            } else if let Err(e) = fs::rename(&tmp_path, &filepath).await {
                                error!("[HLS] [{}] Failed to finalize segment: {}", stream_id_owned, e);
                                let _ = fs::remove_file(&tmp_path).await;
                                false
                            } else {
                                debug!("[HLS] [{}] Wrote segment: {} ({:.2}s, {} bytes)",
                                      stream_id_owned, filename, duration, data.len());
                                true
                            };

                            if write_ok {
                                info!(
                                    "[HLS] [{}] Committed segment {} ({:.2}s, {} bytes)",
                                    stream_id_owned, filename, duration, data.len()
                                );
                                let playlist_content = {
                                    let sessions = sessions_clone.read();
                                    sessions.get(&stream_id_owned).map(|s| {
                                        let mut sess = s.write();
                                        sess.playlist.add_segment(duration, seq);
                                        sess.get_playlist()
                                    })
                                };
                                if let Some(content) = playlist_content {
                                    let m3u8_path = output_dir.join("live.m3u8");
                                    let tmp = output_dir.join("live.m3u8.part");
                                    if fs::write(&tmp, &content).await.is_ok() {
                                        let _ = fs::rename(&tmp, &m3u8_path).await;
                                    }
                                }
                            }
                        }

                        if frame_count % 100 == 0 {
                            debug!("[HLS] [{}] Processed {} frames", stream_id_owned, frame_count);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            "[HLS] [{}] Lagged {} frames — jump to live edge",
                            stream_id_owned, n
                        );
                        drain_broadcast_lag(&mut rx);
                        let sessions = sessions_clone.read();
                        if let Some(session) = sessions.get(&stream_id_owned) {
                            session.write().recover_from_lag();
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!("[HLS] [{}] Broadcast channel closed", stream_id_owned);
                        break;
                    }
                }
            }

            info!("[HLS] [{}] HLS frame processing loop stopped after {} frames", 
                  stream_id_owned, frame_count);

            let mut sessions = sessions_clone.write();
            if sessions.remove(&stream_id_owned).is_some() {
                info!("[HLS] [{}] Session removed after loop exit", stream_id_owned);
            }
            task_aborts.write().remove(&stream_id_owned);
        });

        self.task_aborts
            .write()
            .insert(stream_id.to_string(), handle.abort_handle());
        self.touch(stream_id);

        Ok(())
    }

    /// Get the M3U8 playlist (in-memory, only lists committed segments).
    pub fn get_playlist(&self, stream_id: &str) -> Option<String> {
        self.touch(stream_id);
        let sessions = self.sessions.read();
        let session = sessions.get(stream_id)?;
        let sess = session.read();
        if sess.playlist.segment_count() == 0 {
            return None;
        }
        Some(sess.get_playlist())
    }

    /// Get the segment file path for a stream
    pub fn get_segment_path(&self, stream_id: &str, filename: &str) -> Option<PathBuf> {
        self.touch(stream_id);
        let sessions = self.sessions.read();
        if let Some(s) = sessions.get(stream_id) {
            let path = s.read().output_dir().join(filename);
            if path.exists() {
                return Some(path);
            }
        }
        let path = PathBuf::from(&self.config.output_dir).join(stream_id).join(filename);
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }

    /// Check if a stream has an active HLS session
    pub fn has_stream(&self, stream_id: &str) -> bool {
        let sessions = self.sessions.read();
        sessions.contains_key(stream_id)
    }

    /// List all active HLS streams
    pub fn list_streams(&self) -> Vec<String> {
        let sessions = self.sessions.read();
        sessions.keys().cloned().collect()
    }

    /// Stop HLS generation for a stream
    pub async fn stop_stream(&self, stream_id: &str) -> Result<()> {
        if let Some(handle) = self.task_aborts.write().remove(stream_id) {
            handle.abort();
        }
        self.last_access.write().remove(stream_id);

        let session = {
            let mut sessions = self.sessions.write();
            sessions.remove(stream_id)
        };

        if session.is_some() {
            info!("[HLS] Stopped HLS for stream: {}", stream_id);
        }

        Ok(())
    }

    /// Get the output directory for HLS files
    pub fn output_dir(&self) -> &str {
        &self.config.output_dir
    }
}
