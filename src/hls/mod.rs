/// HLS (HTTP Live Streaming) module
/// Subscribes to streams and generates HLS segments (MPEG-TS) with M3U8 playlists.
pub mod m3u8;
pub mod timing;
pub mod ts_muxer;

use anyhow::Result;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::fs;
use tracing::{debug, error, info, warn};

use self::m3u8::M3u8Generator;
use self::ts_muxer::TsMuxer;
use crate::core::dispatch::DispatchError;
use crate::core::{CodecType, DispatchPolicy, MediaFrame, StreamManager};
use crate::webrtc::{
    annex_b_with_config, h264_util::is_keyframe_annex_b, request_publisher_keyframe,
};

use self::timing::{
    closed_segment_secs as timing_closed_segment_secs, live_pdt, should_split_segment,
    split_threshold_secs, SplitEval,
};
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
            segment_duration: 1.0,
            max_segments: 1,
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
    /// Session-wide AAC frame count for continuous audio PTS
    session_audio_frames: u64,
    /// Last raw video timestamp for segment duration (ms)
    last_video_timestamp: u64,
    /// Session-continuous video mux timeline (ms) in TS — matches ffmpeg HLS first_timestamp.
    session_video_mux_ms: u64,
    /// Session-continuous AAC frame index
    session_audio_mux_frames: u64,
    /// Last publisher video timestamp (session-wide)
    session_last_raw_video: u64,
    /// Session mux ms when the open segment started
    segment_open_mux_ms: u64,
    /// Session mux ms after the last frame in the open segment
    segment_last_mux_ms: u64,
    /// Wall-clock anchor for the segment currently being muxed
    segment_wall_start: Option<Instant>,
    /// Session mux ms when the open segment started (for EXTINF at split).
    segment_open_mux_ms_at_split: u64,
    /// Last video keyframe mux ms in the open segment (for proactive IDR request).
    segment_last_idr_mux_ms: u64,
    /// Wall-clock anchor for session mux PTS (matches PDT; avoids publisher ts drift).
    wall_anchor: Option<Instant>,
    /// PDT epoch: first segment start maps to mux ms 0.
    session_pdt_anchor: Option<SystemTime>,
    /// Emit #EXT-X-DISCONTINUITY on the next committed segment (lag snap)
    pending_discontinuity: bool,
}

/// A completed TS segment ready to write to disk.
struct CompletedSegment {
    data: Vec<u8>,
    filename: String,
    duration: f64,
    seq: u64,
    pdt: SystemTime,
    discontinuity: bool,
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
            session_audio_frames: 0,
            last_video_timestamp: 0,
            session_video_mux_ms: 0,
            session_audio_mux_frames: 0,
            session_last_raw_video: 0,
            segment_open_mux_ms: 0,
            segment_last_mux_ms: 0,
            segment_open_mux_ms_at_split: 0,
            segment_last_idr_mux_ms: 0,
            segment_wall_start: None,
            wall_anchor: None,
            session_pdt_anchor: None,
            pending_discontinuity: false,
        })
    }

    /// Discard partial segment after falling behind; keep timeline + CC continuous.
    fn recover_from_lag(&mut self) {
        self.segment_buffer.clear();
        self.segment_duration_acc = 0.0;
        self.last_video_timestamp = 0;
        self.segment_open_mux_ms = self.session_video_mux_ms;
        self.segment_last_mux_ms = self.session_video_mux_ms;
        self.segment_open_mux_ms_at_split = self.session_video_mux_ms;
        self.segment_last_idr_mux_ms = 0;
        self.segment_wall_start = None;
        self.pending_discontinuity = true;
    }

    fn segment_mux_secs(&self) -> f64 {
        self.segment_last_mux_ms.saturating_sub(
            self.segment_open_mux_ms_at_split
                .max(self.segment_open_mux_ms),
        ) as f64
            / 1000.0
    }

    /// Closed segment EXTINF from mux PTS span (must match TS payload).
    fn closed_segment_secs(&self) -> f64 {
        timing_closed_segment_secs(self.segment_last_mux_ms, self.segment_open_mux_ms_at_split)
    }

    /// New TS file; session PTS/CC continue (ffmpeg treats segments as one timeline).
    fn begin_new_segment(&mut self) {
        self.segment_duration_acc = 0.0;
        self.last_video_timestamp = 0;
        self.segment_open_mux_ms = self.session_video_mux_ms;
        self.segment_open_mux_ms_at_split = self.session_video_mux_ms;
        self.segment_last_idr_mux_ms = 0;
        self.segment_wall_start = None;
        self.muxer.reset_for_new_segment();
    }

    fn ensure_wall_timeline(&mut self) {
        if self.wall_anchor.is_none() {
            self.wall_anchor = Some(Instant::now());
            self.session_pdt_anchor = Some(SystemTime::now());
        }
    }

    fn segment_wall_secs(&self) -> f64 {
        self.segment_wall_start
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0)
    }

    fn mux_timestamp_ms(&mut self, frame: &MediaFrame) -> u64 {
        self.ensure_wall_timeline();
        match frame.codec {
            CodecType::AAC => {
                let pts = self.session_audio_mux_frames * 1024 * 1000 / 44100;
                self.session_audio_mux_frames += 1;
                // Keep audio from running ahead of video.
                pts.min(self.session_video_mux_ms.saturating_add(100))
            }
            CodecType::H264 | CodecType::H265 => {
                let (last_raw, mux_ms) = timing::advance_video_mux_ms(
                    self.session_last_raw_video,
                    self.session_video_mux_ms,
                    frame.timestamp,
                );
                self.session_last_raw_video = last_raw;
                self.session_video_mux_ms = mux_ms;
                self.session_video_mux_ms
            }
            _ => 0,
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
        if frame.codec == CodecType::AAC {
            self.session_audio_frames += 1;
        }

        if !self.primed {
            if !is_hls_video_keyframe(frame) {
                return Ok(None);
            }
            self.primed = true;
            info!(
                "[HLS] [{}] Primed on video keyframe ({} bytes)",
                self.stream_id,
                frame.data.len()
            );
        }

        // Initialize segment with PAT/PMT
        if self.segment_buffer.is_empty() {
            self.segment_wall_start = Some(Instant::now());
            self.segment_open_mux_ms = self.session_video_mux_ms;
            self.segment_open_mux_ms_at_split = self.session_video_mux_ms;
            let has_audio = self.session_audio_frames > 0 || frame.codec == CodecType::AAC;
            let pat_pmt = self.muxer.generate_pat_pmt(true, has_audio);
            self.segment_buffer.extend(pat_pmt);
        }

        // Track segment duration from video timestamps only (audio/video RTMP clocks differ)
        if matches!(frame.codec, CodecType::H264 | CodecType::H265) {
            if self.last_video_timestamp > 0 && frame.timestamp > self.last_video_timestamp {
                let delta_ms = crate::core::media_timestamp_delta_ms(
                    self.last_video_timestamp,
                    frame.timestamp,
                );
                if delta_ms > 0 && delta_ms < 2000 {
                    self.segment_duration_acc += delta_ms as f64 / 1000.0;
                }
            }
            self.last_video_timestamp = frame.timestamp;
        }

        let mux_secs = self.segment_mux_secs();

        let split_threshold = split_threshold_secs(
            self.playlist.target_duration(),
            self.playlist.segment_count(),
        );

        // Ask publisher for IDR before we hit the segment cap (RTSP push may ignore).
        if matches!(frame.codec, CodecType::H264 | CodecType::H265)
            && !is_hls_video_keyframe(frame)
            && mux_secs >= split_threshold * 0.85
            && self.segment_last_idr_mux_ms <= self.segment_open_mux_ms_at_split
            && self.segment_buffer.len() > MIN_SEGMENT_BYTES
        {
            request_publisher_keyframe(&self.stream_id);
        }

        let should_split = should_split_segment(&SplitEval {
            is_keyframe: is_hls_video_keyframe(frame),
            mux_secs,
            publisher_secs: self.segment_duration_acc,
            split_threshold,
            committed_segments: self.playlist.segment_count(),
            has_muxed_media: self.segment_last_mux_ms > self.segment_open_mux_ms_at_split,
            buffer_len: self.segment_buffer.len(),
            min_segment_bytes: MIN_SEGMENT_BYTES,
        });

        let mut completed: Option<CompletedSegment> = None;

        if should_split && self.segment_buffer.len() > MIN_SEGMENT_BYTES {
            let seq = self.playlist.next_sequence();
            let filename = M3u8Generator::segment_filename(seq);
            let duration = self.closed_segment_secs();
            let discontinuity = self.pending_discontinuity;
            self.pending_discontinuity = false;
            let pdt = live_pdt(duration, SystemTime::now());
            let data = std::mem::take(&mut self.segment_buffer);

            if duration > self.playlist.target_duration() * 2.0 {
                warn!(
                    "[HLS] [{}] segment seq={} duration {:.2}s (target {:.1}s) — check encoder GOP",
                    self.stream_id,
                    seq,
                    duration,
                    self.playlist.target_duration()
                );
            } else {
                info!(
                    "[HLS] [{}] segment seq={} duration {:.2}s ({} bytes)",
                    self.stream_id,
                    seq,
                    duration,
                    data.len()
                );
            }

            completed = Some(CompletedSegment {
                data,
                filename,
                duration,
                seq,
                pdt,
                discontinuity,
            });

            // New segment starts with the keyframe below; PAT/PMT (+ discontinuity if lag snap).
            self.begin_new_segment();
            if discontinuity {
                self.muxer.mark_segment_discontinuity();
            }
            let has_audio = self.session_audio_frames > 0;
            let pat_pmt = self.muxer.generate_pat_pmt(true, has_audio);
            self.segment_buffer.extend(pat_pmt);
            self.segment_wall_start = Some(Instant::now());
            // Prime PCR to the keyframe PTS before the first PES (avoids DTS 0 at segment open).
            let mux_frame = self.prepare_frame_for_mux(frame);
            self.segment_last_mux_ms = mux_frame.timestamp;
            self.segment_last_idr_mux_ms = mux_frame.timestamp;
            self.muxer.update_pcr(mux_frame.timestamp);
            let ts_data = self.muxer.frame_to_ts(&mux_frame);
            self.segment_buffer.extend(ts_data);
            return Ok(completed);
        }

        let mux_frame = self.prepare_frame_for_mux(frame);
        self.segment_last_mux_ms = mux_frame.timestamp;
        if is_hls_video_keyframe(frame) {
            self.segment_last_idr_mux_ms = mux_frame.timestamp;
        }
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

#[cfg(test)]
impl HlsSession {
    fn record_completed_segment(&mut self, seg: &CompletedSegment) {
        self.playlist
            .add_segment(seg.duration, seg.seq, seg.pdt, seg.discontinuity);
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
        if self
            .stream_manager
            .get_stream(&stream_id.to_string())
            .is_none()
        {
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
        let target = self.config.segment_duration.ceil().max(1.0) as u64;
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
        self.stream_manager.ensure_stream_hub(stream_id);
        let mut reader = match self
            .stream_manager
            .dispatch_subscribe(stream_id, DispatchPolicy::SequentialFromIdr)
        {
            Some(r) => r,
            None => {
                warn!("[HLS] No StreamHub for stream {}", stream_id);
                return Ok(());
            }
        };

        let stream_id_owned = stream_id.to_string();
        let sessions_clone = self.sessions.clone();
        let stream_manager = self.stream_manager.clone();
        let task_aborts = self.task_aborts.clone();

        let handle = tokio::spawn(async move {
            info!(
                "[HLS] [{}] HLS frame processing loop started",
                stream_id_owned
            );
            let mut frame_count: u64 = 0;

            reader.finish_prime();
            request_publisher_keyframe(&stream_id_owned);

            loop {
                let frames = match reader.recv_batch().await {
                    Ok(f) if !f.is_empty() => f,
                    Ok(_) => continue,
                    Err(DispatchError::Closed) => break,
                };

                if reader.take_muxer_resync() {
                    if let Some(session) = sessions_clone.read().get(&stream_id_owned) {
                        session.write().recover_from_lag();
                    }
                }

                for mut frame in frames {
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
                                    seg.pdt,
                                    seg.discontinuity,
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
                    if let Some((data, filename, duration, seq, pdt, discontinuity, output_dir)) =
                        segment_to_write
                    {
                        let filepath = output_dir.join(&filename);
                        let tmp_path = output_dir.join(format!("{}.part", filename));
                        let write_ok = if let Err(e) = fs::write(&tmp_path, &data).await {
                            error!("[HLS] [{}] Failed to write segment: {}", stream_id_owned, e);
                            false
                        } else if let Err(e) = fs::rename(&tmp_path, &filepath).await {
                            error!(
                                "[HLS] [{}] Failed to finalize segment: {}",
                                stream_id_owned, e
                            );
                            let _ = fs::remove_file(&tmp_path).await;
                            false
                        } else {
                            debug!(
                                "[HLS] [{}] Wrote segment: {} ({:.2}s, {} bytes)",
                                stream_id_owned,
                                filename,
                                duration,
                                data.len()
                            );
                            true
                        };

                        if write_ok {
                            info!(
                                "[HLS] [{}] Committed segment {} ({:.2}s, {} bytes)",
                                stream_id_owned,
                                filename,
                                duration,
                                data.len()
                            );
                            let playlist_update = {
                                let sessions = sessions_clone.read();
                                sessions.get(&stream_id_owned).map(|s| {
                                    let mut sess = s.write();
                                    sess.playlist.add_segment(duration, seq, pdt, discontinuity);
                                    let prune =
                                        seq.saturating_sub(sess.playlist.max_segments() as u64 + 2);
                                    (sess.get_playlist(), prune)
                                })
                            };
                            if let Some((content, prune_before)) = playlist_update {
                                let m3u8_path = output_dir.join("live.m3u8");
                                let tmp = output_dir.join("live.m3u8.part");
                                if fs::write(&tmp, &content).await.is_ok() {
                                    let _ = fs::rename(&tmp, &m3u8_path).await;
                                }
                                prune_old_segments(&output_dir, prune_before).await;
                            }
                        }
                    }

                    if frame_count % 100 == 0 {
                        debug!(
                            "[HLS] [{}] Processed {} frames",
                            stream_id_owned, frame_count
                        );
                    }
                }
            }

            info!(
                "[HLS] [{}] HLS frame processing loop stopped after {} frames",
                stream_id_owned, frame_count
            );

            let mut sessions = sessions_clone.write();
            if sessions.remove(&stream_id_owned).is_some() {
                info!(
                    "[HLS] [{}] Session removed after loop exit",
                    stream_id_owned
                );
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
        let path = PathBuf::from(&self.config.output_dir)
            .join(stream_id)
            .join(filename);
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

/// Remove segment files older than the sliding playlist window.
async fn prune_old_segments(output_dir: &Path, prune_before_seq: u64) {
    if prune_before_seq == 0 {
        return;
    }
    let Ok(mut entries) = fs::read_dir(output_dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(seq) = name
            .strip_prefix("segment_")
            .and_then(|s| s.strip_suffix(".ts"))
            .and_then(|s| s.parse::<u64>().ok())
        else {
            continue;
        };
        if seq < prune_before_seq {
            let _ = fs::remove_file(entry.path()).await;
        }
    }
}

#[cfg(test)]
mod playback_tests {
    use super::*;
    use bytes::Bytes;

    const RTP_BASE: u64 = 2_648_000_000;
    const FRAME_TICKS: u64 = 3600;

    fn annex_b_idr() -> Bytes {
        Bytes::from(vec![0, 0, 0, 1, 0x65, 0x88, 0x84, 0])
    }

    fn annex_b_p() -> Bytes {
        Bytes::from(vec![0, 0, 0, 1, 0x41, 0x9a, 0])
    }

    fn h264_frame(ts: u64, key: bool) -> MediaFrame {
        MediaFrame::new(
            "test".into(),
            0,
            ts,
            if key { annex_b_idr() } else { annex_b_p() },
            key,
            CodecType::H264,
        )
    }

    fn temp_hls_config(suffix: &str) -> HlsConfig {
        let dir =
            std::env::temp_dir().join(format!("vcp_hls_test_{}_{suffix}", std::process::id()));
        HlsConfig {
            enabled: true,
            segment_duration: 1.0,
            max_segments: 6,
            output_dir: dir.to_string_lossy().into(),
        }
    }

    fn push_one_second_gop(session: &mut HlsSession, gop_index: u64) -> Vec<CompletedSegment> {
        let mut completed = Vec::new();
        for f in 0..25u64 {
            let ts = RTP_BASE + (gop_index * 25 + f) * FRAME_TICKS;
            let frame = h264_frame(ts, f == 0);
            if let Ok(Some(seg)) = session.on_frame(&frame) {
                completed.push(seg);
            }
        }
        completed
    }

    fn push_until_segment(session: &mut HlsSession) -> CompletedSegment {
        push_one_second_gop(session, 0);
        let completed = push_one_second_gop(session, 1);
        completed
            .into_iter()
            .last()
            .expect("two 1s GOPs should close one segment")
    }

    #[test]
    fn one_second_gop_splits_near_target_not_three_seconds() {
        let config = temp_hls_config("split");
        let mut session = HlsSession::new("t", &config).unwrap();
        let mut durations = Vec::new();

        for gop in 0..4 {
            for seg in push_one_second_gop(&mut session, gop) {
                durations.push(seg.duration);
            }
        }

        assert!(
            durations.len() >= 3,
            "expected multiple completed segments, got {:?}",
            durations
        );
        for &d in &durations {
            assert!(
                d <= 1.5,
                "segment {:.3}s should track ~1s GOP, not accumulate to 3s",
                d
            );
            assert!(
                (d - 3.0).abs() > 0.1,
                "segment {:.3}s matches stale 3s EXTINF bug",
                d
            );
        }
    }

    #[test]
    fn committed_segment_pdt_aligns_with_extinf() {
        let config = temp_hls_config("pdt");
        let mut session = HlsSession::new("t", &config).unwrap();
        let seg = push_until_segment(&mut session);

        let edge = timing::pdt_to_live_edge_secs(seg.pdt, SystemTime::now());
        assert!(
            (edge - seg.duration).abs() < 0.25,
            "PDT should be ~EXTINF behind wall now (edge={edge:.3}, extinf={:.3})",
            seg.duration
        );
    }

    #[test]
    fn playlist_extinf_matches_committed_segment_duration() {
        let config = temp_hls_config("playlist");
        let mut session = HlsSession::new("t", &config).unwrap();
        let seg = push_until_segment(&mut session);
        session.record_completed_segment(&seg);

        let playlist = session.get_playlist();
        assert!(
            playlist.contains(&format!("#EXTINF:{:.3},", seg.duration)),
            "playlist missing EXTINF for {:.3}s segment:\n{playlist}",
            seg.duration
        );
        assert!(
            playlist.contains("#EXT-X-TARGETDURATION:1"),
            "target duration should stay at 1s, not inflate to 3:\n{playlist}"
        );
    }

    #[test]
    fn mux_pts_continuous_across_segment_splits() {
        let config = temp_hls_config("pts");
        let mut session = HlsSession::new("t", &config).unwrap();

        let mut last_closed_mux_ms = 0u64;
        for gop in 0..3 {
            let completed = push_one_second_gop(&mut session, gop);
            if let Some(seg) = completed.last() {
                let closed_mux_ms = (seg.duration * 1000.0).round() as u64;
                assert!(
                    closed_mux_ms >= last_closed_mux_ms,
                    "closed segment mux ms should not rewind"
                );
                last_closed_mux_ms = closed_mux_ms;
            }
        }

        let ts = RTP_BASE + 3 * 25 * FRAME_TICKS;
        let frame = h264_frame(ts, true);
        let _ = session.on_frame(&frame).unwrap();
        assert!(
            session.session_video_mux_ms >= last_closed_mux_ms,
            "open segment PTS must continue after split (mux={} closed={})",
            session.session_video_mux_ms,
            last_closed_mux_ms
        );
    }
}
