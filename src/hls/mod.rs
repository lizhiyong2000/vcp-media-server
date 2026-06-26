/// HLS (HTTP Live Streaming) module
/// Subscribes to streams and generates HLS segments (MPEG-TS) with M3U8 playlists.
pub mod m3u8;
pub mod ts_muxer;

use std::collections::HashMap;
use std::sync::Arc;
use std::path::{Path, PathBuf};
use parking_lot::RwLock;
use tokio::fs;
use tokio::time::{Duration, Instant};
use tracing::{info, warn, error, debug};
use anyhow::Result;

use crate::core::{StreamManager, MediaFrame, CodecType, StreamId};
use self::m3u8::M3u8Generator;
use self::ts_muxer::TsMuxer;

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
            segment_duration: 4.0,
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
    /// Current segment start time
    segment_start_time: Option<Instant>,
    /// Current segment duration accumulator (from timestamps)
    segment_duration_acc: f64,
    /// Last timestamp seen
    last_timestamp: u64,
    /// Whether we've sent the initial PAT/PMT
    pat_pmt_sent: bool,
    /// Output directory for this stream
    output_dir: PathBuf,
    /// Whether this session is active
    active: bool,
}

impl HlsSession {
    fn new(stream_id: &str, config: &HlsConfig) -> Result<Self> {
        let output_dir = PathBuf::from(&config.output_dir).join(stream_id);
        
        // Create output directory
        std::fs::create_dir_all(&output_dir)?;

        Ok(Self {
            stream_id: stream_id.to_string(),
            muxer: TsMuxer::new(),
            playlist: M3u8Generator::new(config.segment_duration, config.max_segments),
            segment_buffer: Vec::new(),
            segment_start_time: None,
            segment_duration_acc: 0.0,
            last_timestamp: 0,
            pat_pmt_sent: false,
            output_dir,
            active: true,
        })
    }

    /// Process an incoming media frame
    fn on_frame(&mut self, frame: &MediaFrame) -> Result<Vec<u8>> {
        if !self.active {
            return Ok(Vec::new());
        }

        let now = Instant::now();

        // Initialize segment start time
        if self.segment_start_time.is_none() {
            self.segment_start_time = Some(now);
            let pat_pmt = self.muxer.generate_pat_pmt(true, true);
            self.segment_buffer.extend(pat_pmt);
            self.pat_pmt_sent = true;
        }

        // Calculate duration from timestamps
        if self.last_timestamp > 0 && frame.timestamp > self.last_timestamp {
            let delta_ms = (frame.timestamp - self.last_timestamp) as f64;
            let delta_sec = if frame.timestamp > 1000000 {
                delta_ms / 90000.0
            } else {
                delta_ms / 1000.0
            };
            self.segment_duration_acc += delta_sec;
        }
        self.last_timestamp = frame.timestamp;

        // Check if we should start a new segment (on keyframe + duration threshold)
        let should_split = frame.is_keyframe 
            && self.segment_duration_acc >= self.playlist.target_duration();

        let mut segment_data_to_write: Option<Vec<u8>> = None;
        let mut segment_filename: Option<String> = None;
        let mut segment_duration: f64 = 0.0;

        if should_split {
            // Finalize current segment
            if !self.segment_buffer.is_empty() {
                let seq = self.playlist.next_sequence();
                let filename = M3u8Generator::get_segment_filename(seq);
                let duration = self.segment_duration_acc;
                
                self.playlist.add_segment(duration, filename.clone());
                
                segment_data_to_write = Some(std::mem::take(&mut self.segment_buffer));
                segment_filename = Some(filename);
                segment_duration = duration;
            }
            
            // Start new segment with PAT/PMT
            let pat_pmt = self.muxer.generate_pat_pmt(true, true);
            self.segment_buffer.extend(pat_pmt);
            self.segment_duration_acc = 0.0;
            self.segment_start_time = None;
            self.pat_pmt_sent = false;
        }

        // Update PCR
        self.muxer.update_pcr(frame.timestamp);

        // Convert frame to TS packets
        let ts_data = self.muxer.frame_to_ts(frame);
        self.segment_buffer.extend(ts_data);

        // Return segment data if we need to write it
        if let (Some(data), Some(filename)) = (segment_data_to_write, segment_filename) {
            // We'll handle writing in the async wrapper
            Ok(data)
        } else {
            Ok(Vec::new())
        }
    }

    /// Finalize the current segment and return data + metadata for writing
    fn finalize_current_segment(&mut self) -> Option<(Vec<u8>, String, f64)> {
        if self.segment_buffer.is_empty() {
            return None;
        }

        let seq = self.playlist.next_sequence();
        let filename = M3u8Generator::get_segment_filename(seq);
        let duration = self.segment_duration_acc;
        let data = std::mem::take(&mut self.segment_buffer);

        self.playlist.add_segment(duration, filename.clone());

        Some((data, filename, duration))
    }

    /// Write segment data to disk (called from async context)
    async fn write_segment(&self, data: &[u8], filename: &str, duration: f64) -> Result<()> {
        let filepath = self.output_dir.join(filename);
        fs::write(&filepath, data).await?;
        info!("[HLS] [{}] Wrote segment: {} ({:.2}s, {} bytes)", 
              self.stream_id, filename, duration, data.len());

        // Write updated M3U8
        let m3u8_path = self.output_dir.join("live.m3u8");
        let m3u8_content = self.playlist.generate();
        fs::write(&m3u8_path, &m3u8_content).await?;
        Ok(())
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

/// HLS Server that manages HLS sessions for multiple streams
pub struct HlsServer {
    stream_manager: Arc<StreamManager>,
    config: HlsConfig,
    sessions: Arc<RwLock<HashMap<String, Arc<RwLock<HlsSession>>>>>,
}

impl HlsServer {
    pub fn new(stream_manager: Arc<StreamManager>, config: HlsConfig) -> Self {
        Self {
            stream_manager,
            config,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start HLS generation for a stream
    pub async fn start_stream(&self, stream_id: &str) -> Result<()> {
        info!("[HLS] Starting HLS generation for stream: {}", stream_id);

        let session = HlsSession::new(stream_id, &self.config)?;
        let session = Arc::new(RwLock::new(session));

        {
            let mut sessions = self.sessions.write();
            sessions.insert(stream_id.to_string(), session.clone());
        }

        // Subscribe to stream frames
        let rx = match self.stream_manager.subscribe(&stream_id.to_string()) {
            Some(rx) => rx,
            None => {
                warn!("[HLS] No broadcast channel found for stream {}", stream_id);
                return Ok(());
            }
        };

        let stream_id_owned = stream_id.to_string();
        let sessions_clone = self.sessions.clone();

        tokio::spawn(async move {
            info!("[HLS] [{}] HLS frame processing loop started", stream_id_owned);
            let mut frame_count: u64 = 0;

            let mut rx = rx;
            loop {
                match rx.recv().await {
                    Ok(frame) => {
                        frame_count += 1;
                        
                        // Process frame synchronously (no await while holding lock)
                        let segment_to_write = {
                            let session_guard = {
                                let sessions = sessions_clone.read();
                                sessions.get(&stream_id_owned).cloned()
                            };

                            if let Some(session) = session_guard {
                                let mut sess = session.write();
                                let result = sess.on_frame(&frame);
                                match result {
                                    Ok(data) if !data.is_empty() => {
                                        // Segment was finalized
                                        let seg_info = sess.finalize_current_segment();
                                        seg_info.map(|(data, filename, duration)| {
                                            (data, filename, duration, sess.output_dir.clone())
                                        })
                                    }
                                    _ => None,
                                }
                            } else {
                                info!("[HLS] [{}] Session removed, stopping", stream_id_owned);
                                break;
                            }
                        };

                        // Write segment to disk outside of lock
                        if let Some((data, filename, duration, output_dir)) = segment_to_write {
                            let filepath = output_dir.join(&filename);
                            if let Err(e) = fs::write(&filepath, &data).await {
                                error!("[HLS] [{}] Failed to write segment: {}", stream_id_owned, e);
                            } else {
                                info!("[HLS] [{}] Wrote segment: {} ({:.2}s, {} bytes)", 
                                      stream_id_owned, filename, duration, data.len());
                            }
                            // Update M3U8
                            let m3u8_path = output_dir.join("live.m3u8");
                            let m3u8_content = {
                                let sessions = sessions_clone.read();
                                sessions.get(&stream_id_owned).map(|s| s.read().get_playlist())
                            };
                            if let Some(content) = m3u8_content {
                                if let Err(e) = fs::write(&m3u8_path, &content).await {
                                    error!("[HLS] [{}] Failed to write M3U8: {}", stream_id_owned, e);
                                }
                            }
                        }

                        if frame_count % 100 == 0 {
                            debug!("[HLS] [{}] Processed {} frames", stream_id_owned, frame_count);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("[HLS] [{}] Lagged {} frames", stream_id_owned, n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!("[HLS] [{}] Broadcast channel closed", stream_id_owned);
                        break;
                    }
                }
            }

            info!("[HLS] [{}] HLS frame processing loop stopped after {} frames", 
                  stream_id_owned, frame_count);
        });

        Ok(())
    }

    /// Get the M3U8 playlist for a stream
    pub fn get_playlist(&self, stream_id: &str) -> Option<String> {
        let sessions = self.sessions.read();
        sessions.get(stream_id).map(|s| s.read().get_playlist())
    }

    /// Get the segment file path for a stream
    pub fn get_segment_path(&self, stream_id: &str, filename: &str) -> Option<PathBuf> {
        let sessions = self.sessions.read();
        sessions.get(stream_id).map(|s| {
            let sess = s.read();
            sess.output_dir().join(filename)
        })
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
        let session = {
            let mut sessions = self.sessions.write();
            sessions.remove(stream_id)
        };

        if let Some(session) = session {
            let sess = session.write();
            info!("[HLS] Stopped HLS for stream: {}", stream_id);
        }

        Ok(())
    }

    /// Get the output directory for HLS files
    pub fn output_dir(&self) -> &str {
        &self.config.output_dir
    }
}
