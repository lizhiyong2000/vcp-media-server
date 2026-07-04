use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{debug, error, info, trace, warn};

use crate::core::{StreamManager, TranscodeConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscodeProfile {
    pub video_codec: Option<String>,
    pub video_bitrate: Option<String>,
    pub resolution: Option<String>,
    pub fps: Option<u32>,
    pub audio_codec: Option<String>,
    pub audio_bitrate: Option<String>,
    pub audio_sample_rate: Option<u32>,
}

impl Default for TranscodeProfile {
    fn default() -> Self {
        Self {
            video_codec: Some("h264".to_string()),
            video_bitrate: Some("1000k".to_string()),
            resolution: None,
            fps: None,
            audio_codec: Some("aac".to_string()),
            audio_bitrate: Some("64k".to_string()),
            audio_sample_rate: Some(44100),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartTranscodeRequest {
    pub source_stream_id: String,
    pub target_stream_id: String,
    pub profile: Option<TranscodeProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopTranscodeRequest {
    pub source_stream_id: String,
    pub target_stream_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscodeSessionInfo {
    pub source_stream_id: String,
    pub target_stream_id: String,
    pub profile: TranscodeProfile,
    pub status: String,
    pub started_at: u64,
}

struct ActiveTranscodeSession {
    info: TranscodeSessionInfo,
    ffmpeg_child: Option<std::sync::Mutex<Child>>,
    stop_tx: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct TranscodeManager {
    sessions: Arc<RwLock<HashMap<String, ActiveTranscodeSession>>>,
    stream_manager: Arc<StreamManager>,
    config: TranscodeConfig,
    max_workers: usize,
    http_port: u16,
    rtmp_port: u16,
}

impl TranscodeManager {
    pub fn new(
        stream_manager: Arc<StreamManager>,
        config: &TranscodeConfig,
        http_port: u16,
        rtmp_port: u16,
    ) -> Self {
        let max_workers = config.max_workers.unwrap_or(4);

        debug!(
            "[TranscodeManager] initialized with max_workers={}, http_port={}, rtmp_port={}",
            max_workers, http_port, rtmp_port
        );

        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            stream_manager,
            config: config.clone(),
            max_workers,
            http_port,
            rtmp_port,
        }
    }

    pub fn start(&self, req: StartTranscodeRequest) -> Result<TranscodeSessionInfo, String> {
        let start_time = std::time::Instant::now();

        info!(
            "[Transcode] [START] request received: source={}, target={}, profile={:?}",
            req.source_stream_id, req.target_stream_id, req.profile
        );

        if !self.config.enabled {
            warn!("[Transcode] [START] rejected: transcode is disabled");
            return Err("transcode is disabled".to_string());
        }

        let sessions = self.sessions.read();
        if sessions.len() >= self.max_workers {
            warn!(
                "[Transcode] [START] rejected: max workers reached (current={}, max={})",
                sessions.len(), self.max_workers
            );
            return Err(format!("max transcode workers reached: {}", self.max_workers));
        }
        let current_worker_count = sessions.len();
        drop(sessions);

        let profile = req.profile.unwrap_or_default();

        let session_key = format!("{}->{}", req.source_stream_id, req.target_stream_id);

        let mut sessions = self.sessions.write();
        if sessions.contains_key(&session_key) {
            warn!("[Transcode] [START] rejected: session already exists ({})", session_key);
            return Err(format!("transcode session already exists: {}", session_key));
        }

        let started_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let info = TranscodeSessionInfo {
            source_stream_id: req.source_stream_id.clone(),
            target_stream_id: req.target_stream_id.clone(),
            profile: profile.clone(),
            status: "starting".to_string(),
            started_at,
        };

        let (stop_tx, stop_rx) = watch::channel(false);

        sessions.insert(session_key.clone(), ActiveTranscodeSession {
            info: info.clone(),
            ffmpeg_child: None,
            stop_tx,
        });
        drop(sessions);

        info!(
            "[Transcode] [START] session created: key={}, workers={}/{}, started_at={}",
            session_key, current_worker_count + 1, self.max_workers, started_at
        );

        let source_id = req.source_stream_id.clone();
        let target_id = req.target_stream_id.clone();
        let profile_clone = profile.clone();
        let sessions_clone = self.sessions.clone();
        let ffmpeg_path = self.config.ffmpeg_path.clone().unwrap_or("ffmpeg".to_string());
        let http_port = self.http_port;
        let rtmp_port = self.rtmp_port;

        tokio::spawn(async move {
            info!("[Transcode] [WORKER] task spawned: {} -> {}", source_id, target_id);
            
            // RTMP play 作为 ffmpeg 输入在本服务上不稳定；HTTP-FLV 拉流更可靠。
            let source_url =
                format!("http://127.0.0.1:{}/flv/{}?join=idr", http_port, source_id);
            let target_url = format!("rtmp://127.0.0.1:{}/live/{}", rtmp_port, target_id);

            info!("[Transcode] [WORKER] starting ffmpeg transcode process: {} -> {}", source_url, target_url);

            match Self::start_ffmpeg_process(&ffmpeg_path, &source_url, &target_url, &profile_clone) {
                Ok(child) => {
                    let pid = child.id();
                    info!("[Transcode] [WORKER] ffmpeg process started: pid={}", pid);

                    {
                        let mut sessions_write = sessions_clone.write();
                        if let Some(session) = sessions_write.get_mut(&session_key) {
                            session.info.status = "running".to_string();
                            session.ffmpeg_child = Some(std::sync::Mutex::new(child));
                        }
                    }

                    Self::monitor_ffmpeg_process(session_key, sessions_clone, stop_rx).await;
                }
                Err(e) => {
                    error!("[Transcode] [WORKER] failed to start ffmpeg: {}", e);
                    {
                        let mut sessions_write = sessions_clone.write();
                        if let Some(session) = sessions_write.get_mut(&session_key) {
                            session.info.status = "failed".to_string();
                        }
                        sessions_write.remove(&session_key);
                    }
                }
            }
        });

        let elapsed = start_time.elapsed();
        info!(
            "[Transcode] [START] request completed: {} -> {}, profile={:?}, elapsed={:.2?}",
            req.source_stream_id, req.target_stream_id, profile, elapsed
        );
        Ok(info)
    }

    pub fn stop(&self, req: StopTranscodeRequest) -> Result<TranscodeSessionInfo, String> {
        let start_time = std::time::Instant::now();

        info!(
            "[Transcode] [STOP] request received: source={}, target={:?}",
            req.source_stream_id, req.target_stream_id
        );

        let mut sessions = self.sessions.write();

        let session_key = if let Some(target) = &req.target_stream_id {
            format!("{}->{}", req.source_stream_id, target)
        } else {
            let prefix = format!("{}->", req.source_stream_id);
            let key = sessions.keys().find(|k| k.starts_with(&prefix));
            match key {
                Some(k) => {
                    info!("[Transcode] [STOP] found session by source prefix: {} -> {}", req.source_stream_id, k);
                    k.clone()
                }
                None => {
                    warn!(
                        "[Transcode] [STOP] rejected: no session found for source {}",
                        req.source_stream_id
                    );
                    return Err("transcode session not found".to_string());
                }
            }
        };

        info!("[Transcode] [STOP] looking for session: {}", session_key);

        let session = sessions.remove(&session_key);
        match session {
            Some(s) => {
                info!(
                    "[Transcode] [STOP] session found and removed: {} -> {}, status={}, started_at={}",
                    s.info.source_stream_id, s.info.target_stream_id, s.info.status, s.info.started_at
                );

                if let Some(child_mutex) = &s.ffmpeg_child {
                    if let Ok(mut child) = child_mutex.lock() {
                        let pid = child.id();
                        info!("[Transcode] [STOP] killing ffmpeg process: pid={}", pid);
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }

                let send_result = s.stop_tx.send(true);
                if send_result.is_ok() {
                    info!("[Transcode] [STOP] stop signal sent to monitor");
                } else {
                    warn!("[Transcode] [STOP] failed to send stop signal");
                }

                let elapsed = start_time.elapsed();
                info!(
                    "[Transcode] [STOP] request completed: {} -> {}, elapsed={:.2?}",
                    s.info.source_stream_id, s.info.target_stream_id, elapsed
                );
                Ok(s.info)
            }
            None => {
                warn!("[Transcode] [STOP] rejected: session not found ({})", session_key);
                Err("transcode session not found".to_string())
            }
        }
    }

    pub fn active_sessions(&self) -> Vec<TranscodeSessionInfo> {
        let sessions = self.sessions.read();
        let count = sessions.len();
        let result = sessions.values().map(|s| s.info.clone()).collect();
        debug!("[TranscodeManager] active_sessions() called, returning {} sessions", count);
        result
    }

    fn start_ffmpeg_process(ffmpeg_path: &str, source_url: &str, target_url: &str, profile: &TranscodeProfile) -> Result<Child, String> {
        let start_time = std::time::Instant::now();

        let output_codec = profile.video_codec.as_deref().unwrap_or("h264");
        let video_bitrate = profile.video_bitrate.as_deref().unwrap_or("1000k");
        let fps = profile.fps.unwrap_or(30);

        debug!("[Transcode] [FFMPEG] building command: codec={}, bitrate={}, fps={}", 
            output_codec, video_bitrate, fps);

        let fps_str = fps.to_string();
        let gop_str = (fps * 2).to_string();

        let mut args = vec!["-fflags", "+genpts", "-max_delay", "0"];

        if source_url.starts_with("http://") {
            args.extend([
                "-probesize", "32",
                "-analyzeduration", "0",
                "-f", "flv",
            ]);
            debug!("[Transcode] [FFMPEG] using HTTP-FLV input");
        } else {
            args.extend(["-re"]);
        }

        args.extend(["-i", source_url]);

        let codec_name = if output_codec == "h265" || output_codec == "hevc" {
            "libx265"
        } else {
            "libx264"
        };

        args.extend([
            "-c:v", codec_name,
            "-pix_fmt", "yuv420p",
            "-b:v", video_bitrate,
            "-r", &fps_str,
            "-preset", "ultrafast",
            "-tune", "zerolatency",
            "-g", &gop_str,
            "-an",
        ]);

        if let Some(resolution) = &profile.resolution {
            args.push("-s");
            args.push(resolution);
            debug!("[Transcode] [FFMPEG] added resolution: {}", resolution);
        }

        if output_codec == "h265" || output_codec == "hevc" {
            debug!("[Transcode] [FFMPEG] using HEVC codec with libx265");
            args.push("-rtmp_enhanced_codecs");
            args.push("hvc1");
        }

        args.push("-f");
        args.push("flv");
        args.push(target_url);

        info!("[Transcode] [FFMPEG] command: {} {}", ffmpeg_path, args.join(" "));

        let child = Command::new(ffmpeg_path)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                error!("[Transcode] [FFMPEG] failed to spawn process: {}", e);
                format!("failed to spawn ffmpeg: {}", e)
            })?;

        let pid = child.id();
        debug!("[Transcode] [FFMPEG] process spawned successfully: pid={}, elapsed={:.2?}", 
            pid, start_time.elapsed());

        Ok(child)
    }

    async fn monitor_ffmpeg_process(
        session_key: String,
        sessions: Arc<RwLock<HashMap<String, ActiveTranscodeSession>>>,
        mut stop_rx: watch::Receiver<bool>,
    ) {
        let start_time = std::time::Instant::now();

        info!("[Transcode] [MONITOR] started for session: {}", session_key);

        let mut check_interval = tokio::time::interval(std::time::Duration::from_secs(5));

        loop {
            tokio::select! {
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() {
                        info!("[Transcode] [MONITOR] stop signal received for session: {}", session_key);
                        break;
                    }
                }
                _ = check_interval.tick() => {
                    let mut should_remove = false;

                    {
                        let sessions_read = sessions.read();
                        let session = sessions_read.get(&session_key);

                        if let Some(session) = session {
                            if let Some(child_mutex) = &session.ffmpeg_child {
                                match child_mutex.lock() {
                                    Ok(mut child) => {
                                        match child.try_wait() {
                                            Ok(Some(status)) => {
                                                error!("[Transcode] [MONITOR] ffmpeg process exited unexpectedly for session {}: status={}", 
                                                    session_key, status);
                                                should_remove = true;
                                            }
                                            Ok(None) => {
                                                trace!("[Transcode] [MONITOR] ffmpeg process still running for session: {}", session_key);
                                            }
                                            Err(e) => {
                                                warn!("[Transcode] [MONITOR] failed to check ffmpeg status for session {}: {}", 
                                                    session_key, e);
                                            }
                                        }
                                    }
                                    Err(_) => {
                                        warn!("[Transcode] [MONITOR] failed to lock ffmpeg child mutex for session: {}", session_key);
                                    }
                                }
                            } else {
                                warn!("[Transcode] [MONITOR] no ffmpeg process for session: {}", session_key);
                                should_remove = true;
                            }
                        } else {
                            info!("[Transcode] [MONITOR] session {} no longer exists, exiting", session_key);
                            break;
                        }
                    }

                    if should_remove {
                        let mut sessions_write = sessions.write();
                        sessions_write.remove(&session_key);
                        break;
                    }
                }
            }
        }

        let elapsed = start_time.elapsed();
        info!("[Transcode] [MONITOR] stopped for session: {}, elapsed={:.2?}", session_key, elapsed);
    }
}

pub fn parse_bitrate(bitrate: &str) -> Result<i64, String> {
    let s = bitrate.to_lowercase();
    if s.ends_with("k") {
        let num = s.trim_end_matches('k').parse::<i64>()
            .map_err(|e| format!("invalid bitrate: {}", e))?;
        debug!("[parse_bitrate] parsed '{}' as {} bps", bitrate, num * 1000);
        Ok(num * 1000)
    } else if s.ends_with("m") {
        let num = s.trim_end_matches('m').parse::<i64>()
            .map_err(|e| format!("invalid bitrate: {}", e))?;
        debug!("[parse_bitrate] parsed '{}' as {} bps", bitrate, num * 1000 * 1000);
        Ok(num * 1000 * 1000)
    } else {
        let value = bitrate.parse::<i64>()
            .map_err(|e| format!("invalid bitrate: {}", e))?;
        debug!("[parse_bitrate] parsed '{}' as {} bps", bitrate, value);
        Ok(value)
    }
}

pub fn parse_resolution(resolution: &str) -> Result<(u32, u32), String> {
    let parts: Vec<&str> = resolution.split('x').collect();
    if parts.len() != 2 {
        error!("[parse_resolution] invalid format: {}", resolution);
        return Err(format!("invalid resolution format: {}", resolution));
    }
    let width = parts[0].parse::<u32>()
        .map_err(|e| format!("invalid width: {}", e))?;
    let height = parts[1].parse::<u32>()
        .map_err(|e| format!("invalid height: {}", e))?;
    debug!("[parse_resolution] parsed '{}' as {}x{}", resolution, width, height);
    Ok((width, height))
}
