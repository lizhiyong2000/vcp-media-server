//! FFmpeg/FFplay process management for integration tests.
//!
//! Provides utilities for spawning and managing ffmpeg publish and ffplay
//! playback processes during integration testing.

use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use std::sync::Arc;
use parking_lot::Mutex;
use tokio::time::sleep;
use vcp_media_server::core::StreamManager;

use super::media_server_runner::MediaServer;

/// FFmpeg/FFplay process manager for tests.
pub struct ProcessManager {
    ffmpeg_processes: Mutex<Vec<Child>>,
    ffplay_processes: Mutex<Vec<Child>>,
    stream_manager: Arc<StreamManager>,
}

impl ProcessManager {
    /// Create a new process manager.
    pub fn new(stream_manager: Arc<StreamManager>) -> Self {
        Self {
            ffmpeg_processes: Mutex::new(Vec::new()),
            ffplay_processes: Mutex::new(Vec::new()),
            stream_manager,
        }
    }

    /// Spawn ffmpeg publish process.
    pub fn spawn_ffmpeg_publish(&self, args: &[String]) -> Result<(), String> {
        let child = spawn_ffmpeg(args).map_err(|e| format!("Failed to spawn ffmpeg: {}", e))?;

        // Give ffmpeg a moment to start
        std::thread::sleep(Duration::from_millis(100));

        self.ffmpeg_processes.lock().push(child);
        Ok(())
    }

    /// Spawn ffplay playback process.
    pub fn spawn_ffplay(&self, args: &[String]) -> Result<(), String> {
        let child = spawn_ffplay(args).map_err(|e| format!("Failed to spawn ffplay: {}", e))?;

        self.ffplay_processes.lock().push(child);
        Ok(())
    }

    /// Check if stream is active on the server.
    pub fn is_stream_active(&self, stream_id: &str) -> bool {
        if let Some(hub) = self.stream_manager.get_hub(stream_id) {
            return hub.latest_seq() > 0;
        }
        false
    }

    /// Wait for stream to become active with timeout.
    pub async fn wait_for_stream_active(&self, stream_id: &str, timeout_secs: u64) -> bool {
        let start = Instant::now();
        while start.elapsed().as_secs() < timeout_secs {
            if self.is_stream_active(stream_id) {
                return true;
            }
            sleep(Duration::from_millis(50)).await;
        }
        false
    }

    /// Get stream statistics.
    pub fn get_stream_stats(&self, stream_id: &str) -> Option<StreamStats> {
        self.stream_manager.get_hub(stream_id).map(|hub| StreamStats {
            latest_seq: hub.latest_seq(),
        })
    }

    /// Cleanup all processes.
    pub fn cleanup(&self) {
        // Kill all ffmpeg processes
        let mut ffmpegs = self.ffmpeg_processes.lock();
        for mut child in ffmpegs.drain(..) {
            let _ = stop_child(&mut child);
        }

        // Kill all ffplay processes
        let mut ffplays = self.ffplay_processes.lock();
        for mut child in ffplays.drain(..) {
            let _ = stop_child(&mut child);
        }
    }
}

/// Spawn ffmpeg without piped stdio (avoids pipe-buffer deadlocks in tests).
pub fn spawn_ffmpeg(args: &[String]) -> std::io::Result<Child> {
    Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Spawn ffplay without piped stdio.
pub fn spawn_ffplay(args: &[String]) -> std::io::Result<Child> {
    Command::new("ffplay")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Terminate a child process and wait for it to exit (with bounded wait).
pub fn stop_child(child: &mut Child) -> std::io::Result<ExitStatus> {
    let _ = child.kill();
    for _ in 0..30 {
        if let Ok(Some(status)) = child.try_wait() {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child.wait()
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Stream statistics.
#[derive(Debug, Clone)]
pub struct StreamStats {
    pub latest_seq: u64,
}

/// FFmpeg publish configuration for integration tests.
#[derive(Debug, Clone)]
pub struct PublishConfig {
    pub stream_url: String,
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub duration_secs: u32,
    pub codec: PublishCodec,
    /// RTSP transport mode (only used when publishing to RTSP).
    pub rtsp_transport: Option<RtspTransport>,
}

#[derive(Debug, Clone, Copy)]
pub enum PublishCodec {
    H264,
    H265,
}

impl Default for PublishConfig {
    fn default() -> Self {
        Self {
            stream_url: String::new(),
            width: 640,
            height: 480,
            framerate: 25,
            duration_secs: 10,
            codec: PublishCodec::H264,
            rtsp_transport: None,
        }
    }
}

impl PublishConfig {
    /// Create RTMP publish config.
    pub fn rtmp(host: &str, port: u16, app: &str, stream_key: &str) -> Self {
        Self {
            stream_url: format!("rtmp://{}:{}/{}/{}", host, port, app, stream_key),
            ..Default::default()
        }
    }

    /// Create RTSP publish config.
    pub fn rtsp(host: &str, port: u16, stream_id: &str) -> Self {
        Self {
            stream_url: format!("rtsp://{}:{}/{}", host, port, stream_id),
            ..Default::default()
        }
    }

    /// Set resolution.
    pub fn resolution(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Set framerate.
    pub fn framerate(mut self, fps: u32) -> Self {
        self.framerate = fps;
        self
    }

    /// Set duration.
    pub fn duration(mut self, secs: u32) -> Self {
        self.duration_secs = secs;
        self
    }

    /// Set codec.
    pub fn codec(mut self, codec: PublishCodec) -> Self {
        self.codec = codec;
        self
    }

    /// Force RTSP TCP transport for publish.
    pub fn rtsp_transport_tcp(mut self) -> Self {
        self.rtsp_transport = Some(RtspTransport::Tcp);
        self
    }

    /// Force RTSP UDP transport for publish.
    pub fn rtsp_transport_udp(mut self) -> Self {
        self.rtsp_transport = Some(RtspTransport::Udp);
        self
    }

    /// Build ffmpeg command arguments.
    pub fn ffmpeg_args(&self) -> Vec<String> {
        let codec_name = match self.codec {
            PublishCodec::H264 => "libx264",
            PublishCodec::H265 => "libx265",
        };

        let (output_format, url) = if self.stream_url.starts_with("rtmp://") {
            ("flv", self.stream_url.as_str())
        } else if self.stream_url.starts_with("rtsp://") {
            ("rtsp", self.stream_url.as_str())
        } else {
            ("mpegts", self.stream_url.as_str())
        };

        let mut args = vec![
            "-re".to_string(),
            "-f".to_string(),
            "lavfi".to_string(),
            "-i".to_string(),
            format!("testsrc2=size={}x{}:rate={}", self.width, self.height, self.framerate),
            "-f".to_string(),
            "lavfi".to_string(),
            "-i".to_string(),
            "aevalsrc=0.1*sin(2*PI*440*t):sample_rate=48000".to_string(),
        ];

        // RTSP transport mode for publish
        if output_format == "rtsp" {
            if let Some(transport) = &self.rtsp_transport {
                args.push("-rtsp_transport".to_string());
                args.push(match transport {
                    RtspTransport::Tcp => "tcp",
                    RtspTransport::Udp => "udp",
                }
                .to_string());
            }
        }

        args.push("-c:v".to_string());
        args.push(codec_name.to_string());

        if matches!(self.codec, PublishCodec::H264) {
            args.push("-g".to_string());
            args.push("50".to_string());
            args.push("-keyint_min".to_string());
            args.push("50".to_string());
        }

        if matches!(self.codec, PublishCodec::H265) {
            args.push("-preset".to_string());
            args.push("ultrafast".to_string());
        }

        args.extend([
            "-c:a".to_string(),
            "aac".to_string(),
            "-t".to_string(),
            self.duration_secs.to_string(),
            "-f".to_string(),
            output_format.to_string(),
        ]);

        if output_format == "flv" && matches!(self.codec, PublishCodec::H265) {
            args.push("-rtmp_enhanced_codecs".to_string());
            args.push("hvc1".to_string());
        }

        args.push("-y".to_string());
        args.push(url.to_string());

        args
    }
}

/// Playback configuration.
#[derive(Debug, Clone)]
pub struct PlayConfig {
    pub stream_url: String,
    pub duration_secs: u32,
    pub protocol: PlayProtocol,
    /// Enable Enhanced RTMP HEVC (`-rtmp_enhanced_codecs hvc1`) for ffplay.
    pub enhanced_rtmp_hevc: bool,
    /// Force RTSP transport for ffplay (`-rtsp_transport tcp|udp`).
    pub rtsp_transport: Option<RtspTransport>,
}

#[derive(Debug, Clone, Copy)]
pub enum RtspTransport {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Copy)]
pub enum PlayProtocol {
    RTMP,
    RTSP,
    HTTP_FLV,
    HLS,
}

impl PlayConfig {
    pub fn rtmp(url: &str) -> Self {
        Self {
            stream_url: url.to_string(),
            duration_secs: 3,
            protocol: PlayProtocol::RTMP,
            enhanced_rtmp_hevc: false,
            rtsp_transport: None,
        }
    }

    pub fn rtsp(url: &str) -> Self {
        Self {
            stream_url: url.to_string(),
            duration_secs: 3,
            protocol: PlayProtocol::RTSP,
            enhanced_rtmp_hevc: false,
            rtsp_transport: None,
        }
    }

    pub fn http_flv(url: &str) -> Self {
        Self {
            stream_url: url.to_string(),
            duration_secs: 3,
            protocol: PlayProtocol::HTTP_FLV,
            enhanced_rtmp_hevc: false,
            rtsp_transport: None,
        }
    }

    pub fn hls(url: &str) -> Self {
        Self {
            stream_url: url.to_string(),
            duration_secs: 5,
            protocol: PlayProtocol::HLS,
            enhanced_rtmp_hevc: false,
            rtsp_transport: None,
        }
    }

    pub fn duration(mut self, secs: u32) -> Self {
        self.duration_secs = secs;
        self
    }

    pub fn enhanced_rtmp_hevc(mut self) -> Self {
        self.enhanced_rtmp_hevc = true;
        self
    }

    pub fn rtsp_transport_tcp(mut self) -> Self {
        self.rtsp_transport = Some(RtspTransport::Tcp);
        self
    }

    pub fn rtsp_transport_udp(mut self) -> Self {
        self.rtsp_transport = Some(RtspTransport::Udp);
        self
    }

    pub fn ffplay_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.enhanced_rtmp_hevc {
            args.push("-rtmp_enhanced_codecs".to_string());
            args.push("hvc1".to_string());
        }
        if let Some(transport) = self.rtsp_transport {
            args.push("-rtsp_transport".to_string());
            args.push(match transport {
                RtspTransport::Tcp => "tcp",
                RtspTransport::Udp => "udp",
            }
            .to_string());
        }
        if matches!(self.protocol, PlayProtocol::HLS) {
            args.push("-probesize".to_string());
            args.push("500000".to_string());
            args.push("-analyzeduration".to_string());
            args.push("2000000".to_string());
        }
        args.extend([
            "-i".to_string(),
            self.stream_url.clone(),
            "-t".to_string(),
            self.duration_secs.to_string(),
            "-autoexit".to_string(),
            "-nodisp".to_string(),
            "-v".to_string(),
            "error".to_string(),
        ]);
        args
    }
}

/// Wait until ffmpeg is still publishing after warm-up.
pub async fn assert_ffmpeg_publishing(
    ffmpeg: &mut Child,
    warm_up: Duration,
) -> Result<(), String> {
    sleep(warm_up).await;
    match ffmpeg.try_wait().map_err(|e| e.to_string())? {
        None => Ok(()),
        Some(status) => Err(format!("ffmpeg exited before playback: {status}")),
    }
}

/// Run ffplay and verify it stays alive while receiving media.
pub async fn run_ffplay_playback(play: &PlayConfig) -> Result<(), String> {
    if !is_ffplay_available() {
        return Err("ffplay not available".into());
    }

    let mut ffplay = spawn_ffplay(&play.ffplay_args()).map_err(|e| e.to_string())?;
    let play_for = Duration::from_secs(play.duration_secs as u64);
    let started = Instant::now();

    sleep(Duration::from_millis(500)).await;
    if let Some(status) = ffplay.try_wait().map_err(|e| e.to_string())? {
        return Err(format!("ffplay exited before playback: {status}"));
    }

    loop {
        if let Some(status) = ffplay.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("ffplay failed: {status}"))
            };
        }
        if started.elapsed() >= play_for {
            let _ = stop_child(&mut ffplay);
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// Publish with ffmpeg, verify HLS playback with ffplay, then stop publisher.
pub async fn publish_and_hls_playback(
    server: &MediaServer,
    stream_id: &str,
    publish: &PublishConfig,
    play: &PlayConfig,
) -> Result<(), String> {
    let mut ffmpeg = spawn_ffmpeg(&publish.ffmpeg_args()).map_err(|e| e.to_string())?;
    assert_ffmpeg_publishing(&mut ffmpeg, Duration::from_millis(2000)).await?;

    server.trigger_hls(stream_id).await?;
    // Allow time for the first TS segment to be muxed (segment_duration=1 in test config).
    sleep(Duration::from_secs(6)).await;

    let play_result = tokio::time::timeout(
        Duration::from_secs(play.duration_secs as u64 + 8),
        run_ffplay_playback(play),
    )
    .await
    .map_err(|_| "ffplay HLS playback timed out".to_string())?;

    let _ = stop_child(&mut ffmpeg);
    play_result
}

/// Publish source, trigger HTTP API pull, ffplay pulled stream, then stop publisher.
pub async fn publish_api_pull_and_play(
    server: &MediaServer,
    publish: &PublishConfig,
    remote_url: &str,
    pulled_stream_id: &str,
    rtsp_pull: bool,
    play: &PlayConfig,
) -> Result<(), String> {
    let mut ffmpeg = spawn_ffmpeg(&publish.ffmpeg_args()).map_err(|e| e.to_string())?;
    assert_ffmpeg_publishing(&mut ffmpeg, Duration::from_millis(2000)).await?;

    let resp = if rtsp_pull {
        server.api_rtsp_pull(remote_url, pulled_stream_id).await?
    } else {
        server.api_rtmp_pull(remote_url, pulled_stream_id).await?
    };
    if resp.status != 200 {
        let _ = stop_child(&mut ffmpeg);
        return Err(format!(
            "pull API failed: status {} body {}",
            resp.status, resp.body
        ));
    }

    sleep(Duration::from_secs(3)).await;
    let play_result = tokio::time::timeout(
        Duration::from_secs(play.duration_secs as u64 + 8),
        run_ffplay_playback(play),
    )
    .await
    .map_err(|_| "ffplay pull playback timed out".to_string())?;
    let _ = stop_child(&mut ffmpeg);
    play_result
}

/// Publish with ffmpeg, verify playback with ffplay, then stop publisher.
pub async fn publish_and_playback(
    publish: &PublishConfig,
    play: &PlayConfig,
) -> Result<(), String> {
    let mut ffmpeg = spawn_ffmpeg(&publish.ffmpeg_args()).map_err(|e| e.to_string())?;
    assert_ffmpeg_publishing(&mut ffmpeg, Duration::from_millis(2000)).await?;
    let play_result = run_ffplay_playback(play).await;
    let _ = stop_child(&mut ffmpeg);
    play_result
}

/// Check if ffmpeg is available.
pub fn is_ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Check if ffplay is available.
pub fn is_ffplay_available() -> bool {
    Command::new("ffplay")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}
