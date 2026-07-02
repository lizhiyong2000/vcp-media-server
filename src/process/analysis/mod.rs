use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tracing::{debug, error, info, warn};

use crate::core::live_play::prepend_h264_config;
use crate::core::{
    media_frame_timestamp_delta_ms, CodecType, DispatchError, DispatchPolicy, MediaFrame,
    StreamManager,
};

const MAX_EVENTS_PER_STREAM: usize = 256;
const L1_METRICS_PLUGIN: &str = "l1_metrics";
const FACE_DETECTION_PLUGIN: &str = "face_detection";

#[derive(Debug, Clone)]
pub struct AnalysisConfig {
    pub enabled: bool,
    pub default_sample_interval: u64,
    pub max_events_per_stream: usize,
    pub ffmpeg_path: String,
    pub face_detection_dir: PathBuf,
    pub face_detection_interval: Duration,
    pub face_detector_command: Option<String>,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_sample_interval: 1,
            max_events_per_stream: MAX_EVENTS_PER_STREAM,
            ffmpeg_path: "ffmpeg".to_string(),
            face_detection_dir: PathBuf::from("./analysis"),
            face_detection_interval: Duration::from_secs(1),
            face_detector_command: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartAnalysisRequest {
    pub stream_id: String,
    pub plugins: Option<Vec<String>>,
    pub sample_interval: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StopAnalysisRequest {
    pub stream_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalysisSessionInfo {
    pub stream_id: String,
    pub started_at_ms: u64,
    pub sample_interval: u64,
    pub plugins: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AnalysisMetrics {
    pub stream_id: String,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
    pub total_frames: u64,
    pub video_frames: u64,
    pub audio_frames: u64,
    pub keyframes: u64,
    pub bytes: u64,
    pub bitrate_bps: u64,
    pub video_fps: f64,
    pub audio_fps: f64,
    pub last_gop_frames: u64,
    pub avg_gop_frames: f64,
    pub last_video_timestamp: Option<u64>,
    pub last_audio_timestamp: Option<u64>,
    pub last_video_codec: Option<String>,
    pub last_audio_codec: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalysisEvent {
    pub stream_id: String,
    pub timestamp_ms: u64,
    pub kind: String,
    pub message: String,
    pub frame_timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

struct ActiveAnalysisSession {
    info: AnalysisSessionInfo,
    abort: tokio::task::AbortHandle,
}

#[derive(Clone)]
pub struct AnalysisManager {
    stream_manager: Arc<StreamManager>,
    config: AnalysisConfig,
    active: Arc<RwLock<HashMap<String, ActiveAnalysisSession>>>,
    metrics: Arc<RwLock<HashMap<String, AnalysisMetrics>>>,
    events: Arc<RwLock<HashMap<String, VecDeque<AnalysisEvent>>>>,
}

impl AnalysisManager {
    pub fn new(stream_manager: Arc<StreamManager>, config: AnalysisConfig) -> Self {
        Self {
            stream_manager,
            config,
            active: Arc::new(RwLock::new(HashMap::new())),
            metrics: Arc::new(RwLock::new(HashMap::new())),
            events: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn active_sessions(&self) -> Vec<AnalysisSessionInfo> {
        self.active
            .read()
            .values()
            .map(|session| session.info.clone())
            .collect()
    }

    pub fn metrics(&self, stream_id: &str) -> Option<AnalysisMetrics> {
        self.metrics.read().get(stream_id).cloned()
    }

    pub fn events(&self, stream_id: &str) -> Vec<AnalysisEvent> {
        self.events
            .read()
            .get(stream_id)
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn start(&self, req: StartAnalysisRequest) -> Result<AnalysisSessionInfo> {
        if !self.config.enabled {
            return Err(anyhow!("analysis is disabled"));
        }
        if req.stream_id.trim().is_empty() {
            return Err(anyhow!("missing stream_id"));
        }
        if self.stream_manager.get_stream(&req.stream_id).is_none() {
            return Err(anyhow!("stream not found"));
        }
        if self.active.read().contains_key(&req.stream_id) {
            return Err(anyhow!("analysis already active for stream"));
        }

        let info = AnalysisSessionInfo {
            stream_id: req.stream_id.clone(),
            started_at_ms: now_ms(),
            sample_interval: req
                .sample_interval
                .unwrap_or(self.config.default_sample_interval)
                .max(1),
            plugins: req
                .plugins
                .unwrap_or_else(|| vec!["l1_metrics".to_string()]),
        };
        validate_plugins(&info.plugins, &self.config)?;

        let task = AnalysisTask {
            stream_manager: self.stream_manager.clone(),
            info: info.clone(),
            metrics: self.metrics.clone(),
            events: self.events.clone(),
            max_events: self.config.max_events_per_stream,
            config: self.config.clone(),
        };
        let handle = tokio::spawn(async move {
            if let Err(err) = task.run().await {
                error!("[Analysis] pipeline failed: {err:?}");
            }
        });

        self.active.write().insert(
            info.stream_id.clone(),
            ActiveAnalysisSession {
                info: info.clone(),
                abort: handle.abort_handle(),
            },
        );
        Ok(info)
    }

    pub fn stop(&self, req: StopAnalysisRequest) -> Result<AnalysisSessionInfo> {
        let session = self
            .active
            .write()
            .remove(&req.stream_id)
            .ok_or_else(|| anyhow!("analysis session not found"))?;
        session.abort.abort();
        self.metrics.write().remove(&req.stream_id);
        self.events.write().remove(&req.stream_id);
        Ok(session.info)
    }
}

struct AnalysisTask {
    stream_manager: Arc<StreamManager>,
    info: AnalysisSessionInfo,
    metrics: Arc<RwLock<HashMap<String, AnalysisMetrics>>>,
    events: Arc<RwLock<HashMap<String, VecDeque<AnalysisEvent>>>>,
    max_events: usize,
    config: AnalysisConfig,
}

impl AnalysisTask {
    async fn run(self) -> Result<()> {
        info!(
            "[Analysis] start stream='{}' sample_interval={} plugins={:?}",
            self.info.stream_id, self.info.sample_interval, self.info.plugins
        );
        self.stream_manager.ensure_stream_hub(&self.info.stream_id);
        let mut reader = self
            .stream_manager
            .dispatch_subscribe(&self.info.stream_id, DispatchPolicy::LiveSequential)
            .ok_or_else(|| anyhow!("stream hub not available"))?;

        let mut plugins = self.build_plugins()?;
        let context = AnalysisPluginContext {
            stream_manager: self.stream_manager.clone(),
            stream_id: self.info.stream_id.clone(),
        };
        let mut frame_index = 0_u64;
        loop {
            let frames = match reader.recv_batch().await {
                Ok(frames) if !frames.is_empty() => frames,
                Ok(_) => continue,
                Err(DispatchError::Closed) => break,
            };
            if reader.take_live_snap() || reader.take_muxer_resync() {
                self.push_event(AnalysisEvent {
                    stream_id: self.info.stream_id.clone(),
                    timestamp_ms: now_ms(),
                    kind: "resync".to_string(),
                    message: "reader resynced after ring gap or live snap".to_string(),
                    frame_timestamp: 0,
                    data: None,
                });
            }
            for frame in frames {
                let sampled = frame_index % self.info.sample_interval == 0;
                frame_index = frame_index.saturating_add(1);
                for plugin in plugins.iter_mut() {
                    if !sampled && !plugin.process_unsampled_frames() {
                        continue;
                    }
                    let plugin_name = plugin.name();
                    let failure_kind = plugin.failure_event_kind();
                    match plugin.process_frame(&context, &frame).await {
                        Ok(output) => {
                            if let Some(metrics) = output.metrics {
                                self.metrics
                                    .write()
                                    .insert(self.info.stream_id.clone(), metrics);
                            }
                            for event in output.events {
                                self.push_event(event);
                            }
                        }
                        Err(err) => {
                            warn!(
                                "[Analysis] plugin failed stream='{}' plugin='{}' ts={}: {}",
                                self.info.stream_id, plugin_name, frame.timestamp, err
                            );
                            self.push_event(AnalysisEvent {
                                stream_id: self.info.stream_id.clone(),
                                timestamp_ms: now_ms(),
                                kind: failure_kind.to_string(),
                                message: err.to_string(),
                                frame_timestamp: frame.timestamp,
                                data: Some(serde_json::json!({
                                    "plugin": plugin_name,
                                })),
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn push_event(&self, event: AnalysisEvent) {
        let mut events = self.events.write();
        let stream_events = events
            .entry(self.info.stream_id.clone())
            .or_insert_with(VecDeque::new);
        while stream_events.len() >= self.max_events {
            stream_events.pop_front();
        }
        stream_events.push_back(event);
    }

    fn build_plugins(&self) -> Result<Vec<Box<dyn AnalysisPlugin>>> {
        let mut plugins: Vec<Box<dyn AnalysisPlugin>> = vec![Box::new(L1MetricsPlugin::new(
            &self.info.stream_id,
            self.info.started_at_ms,
            self.info.sample_interval,
        ))];

        for plugin in &self.info.plugins {
            match plugin.as_str() {
                L1_METRICS_PLUGIN => {}
                FACE_DETECTION_PLUGIN => {
                    let command = self.config.face_detector_command.clone().ok_or_else(|| {
                        anyhow!("face_detection plugin requires face_detector_command")
                    })?;
                    plugins.push(Box::new(FaceDetectionPlugin::new(
                        FaceDetector {
                            ffmpeg_path: self.config.ffmpeg_path.clone(),
                            work_dir: self.config.face_detection_dir.clone(),
                            min_interval: self.config.face_detection_interval,
                            last_run_ms: None,
                        },
                        Box::new(ExternalCommandVisionBackend { command }),
                    )));
                }
                unsupported => return Err(anyhow!("unsupported analysis plugin: {unsupported}")),
            }
        }

        Ok(plugins)
    }
}

fn validate_plugins(plugins: &[String], config: &AnalysisConfig) -> Result<()> {
    for plugin in plugins {
        match plugin.as_str() {
            L1_METRICS_PLUGIN => {}
            FACE_DETECTION_PLUGIN => {
                if config.face_detector_command.is_none() {
                    return Err(anyhow!(
                        "face_detection plugin requires face_detector_command"
                    ));
                }
            }
            unsupported => return Err(anyhow!("unsupported analysis plugin: {unsupported}")),
        }
    }
    Ok(())
}

struct AnalysisPluginContext {
    stream_manager: Arc<StreamManager>,
    stream_id: String,
}

#[derive(Default)]
struct AnalysisPluginOutput {
    events: Vec<AnalysisEvent>,
    metrics: Option<AnalysisMetrics>,
}

impl AnalysisPluginOutput {
    fn event(event: AnalysisEvent) -> Self {
        Self {
            events: vec![event],
            metrics: None,
        }
    }

    fn metrics(metrics: AnalysisMetrics) -> Self {
        Self {
            events: Vec::new(),
            metrics: Some(metrics),
        }
    }
}

#[async_trait]
trait AnalysisPlugin: Send {
    fn name(&self) -> &'static str;

    fn process_unsampled_frames(&self) -> bool {
        false
    }

    fn failure_event_kind(&self) -> &'static str {
        "analysis_plugin_failed"
    }

    async fn process_frame(
        &mut self,
        context: &AnalysisPluginContext,
        frame: &MediaFrame,
    ) -> Result<AnalysisPluginOutput>;
}

struct L1MetricsPlugin {
    state: MetricsState,
    sample_interval: u64,
}

impl L1MetricsPlugin {
    fn new(stream_id: &str, started_at_ms: u64, sample_interval: u64) -> Self {
        Self {
            state: MetricsState::new(stream_id, started_at_ms),
            sample_interval: sample_interval.max(1),
        }
    }
}

#[async_trait]
impl AnalysisPlugin for L1MetricsPlugin {
    fn name(&self) -> &'static str {
        L1_METRICS_PLUGIN
    }

    fn process_unsampled_frames(&self) -> bool {
        true
    }

    async fn process_frame(
        &mut self,
        _context: &AnalysisPluginContext,
        frame: &MediaFrame,
    ) -> Result<AnalysisPluginOutput> {
        if self.state.metrics.total_frames % self.sample_interval != 0 {
            self.state.observe_skipped(frame);
            return Ok(AnalysisPluginOutput::metrics(self.state.snapshot()));
        }

        let event = self.state.observe(frame);
        let mut output = AnalysisPluginOutput::metrics(self.state.snapshot());
        if let Some(event) = event {
            output.events.push(event);
        }
        Ok(output)
    }
}

struct FaceDetectionPlugin {
    detector: FaceDetector,
    backend: Box<dyn VisionBackend>,
}

impl FaceDetectionPlugin {
    fn new(detector: FaceDetector, backend: Box<dyn VisionBackend>) -> Self {
        Self { detector, backend }
    }
}

#[async_trait]
impl AnalysisPlugin for FaceDetectionPlugin {
    fn name(&self) -> &'static str {
        FACE_DETECTION_PLUGIN
    }

    fn failure_event_kind(&self) -> &'static str {
        "face_detection_failed"
    }

    async fn process_frame(
        &mut self,
        context: &AnalysisPluginContext,
        frame: &MediaFrame,
    ) -> Result<AnalysisPluginOutput> {
        if !self.detector.should_detect(frame) {
            return Ok(AnalysisPluginOutput::default());
        }
        self.detector
            .detect(
                &*context.stream_manager,
                &context.stream_id,
                frame,
                &*self.backend,
            )
            .await
            .map(|event| {
                event.map_or_else(AnalysisPluginOutput::default, AnalysisPluginOutput::event)
            })
    }
}

struct FaceDetector {
    ffmpeg_path: String,
    work_dir: PathBuf,
    min_interval: Duration,
    last_run_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct FaceBox {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct FaceDetectionOutput {
    faces: Vec<FaceBox>,
}

impl FaceDetector {
    fn should_detect(&mut self, frame: &MediaFrame) -> bool {
        if frame.codec != CodecType::H264 || !frame.is_keyframe {
            debug!(
                "[Analysis][FaceDetection] skip frame stream='{}' ts={} codec={:?} keyframe={} reason=unsupported_or_non_keyframe",
                frame.stream_id, frame.timestamp, frame.codec, frame.is_keyframe
            );
            return false;
        }
        let now = now_ms();
        if let Some(last) = self.last_run_ms {
            let elapsed = now.saturating_sub(last);
            let min_interval = self.min_interval.as_millis() as u64;
            if elapsed < min_interval {
                debug!(
                    "[Analysis][FaceDetection] skip frame stream='{}' ts={} reason=interval elapsed_ms={} min_interval_ms={}",
                    frame.stream_id, frame.timestamp, elapsed, min_interval
                );
                return false;
            }
        }
        self.last_run_ms = Some(now);
        debug!(
            "[Analysis][FaceDetection] schedule detection stream='{}' ts={} bytes={}",
            frame.stream_id,
            frame.timestamp,
            frame.data.len()
        );
        true
    }

    async fn detect(
        &self,
        stream_manager: &StreamManager,
        stream_id: &str,
        frame: &MediaFrame,
        backend: &dyn VisionBackend,
    ) -> Result<Option<AnalysisEvent>> {
        let started_at = SystemTime::now();
        info!(
            "[Analysis][FaceDetection] start stream='{}' ts={} backend='{}'",
            stream_id,
            frame.timestamp,
            backend.name()
        );
        let image_path = self
            .write_detection_image(stream_manager, stream_id, frame)
            .await?;
        debug!(
            "[Analysis][FaceDetection] running detector stream='{}' ts={} image='{}'",
            stream_id,
            frame.timestamp,
            image_path.display()
        );
        let faces = backend
            .detect_faces(VisionRequest {
                stream_id,
                frame_timestamp: frame.timestamp,
                image_path: &image_path,
            })
            .await;
        let _ = tokio::fs::remove_file(&image_path).await;
        let faces = faces?;
        let elapsed_ms = started_at.elapsed().unwrap_or_default().as_millis();
        if faces.is_empty() {
            info!(
                "[Analysis][FaceDetection] no face detected stream='{}' ts={} elapsed_ms={}",
                stream_id, frame.timestamp, elapsed_ms
            );
            return Ok(None);
        }
        info!(
            "[Analysis][FaceDetection] faces detected stream='{}' ts={} count={} elapsed_ms={}",
            stream_id,
            frame.timestamp,
            faces.len(),
            elapsed_ms
        );

        Ok(Some(AnalysisEvent {
            stream_id: stream_id.to_string(),
            timestamp_ms: now_ms(),
            kind: "face_detected".to_string(),
            message: format!("detected {} face(s)", faces.len()),
            frame_timestamp: frame.timestamp,
            data: Some(serde_json::json!({
                "face_count": faces.len(),
                "faces": faces,
            })),
        }))
    }

    async fn write_detection_image(
        &self,
        stream_manager: &StreamManager,
        stream_id: &str,
        frame: &MediaFrame,
    ) -> Result<PathBuf> {
        let now = now_ms();
        let dir = self.work_dir.join(sanitize_path_component(stream_id));
        debug!(
            "[Analysis][FaceDetection] prepare image dir stream='{}' ts={} dir='{}'",
            stream_id,
            frame.timestamp,
            dir.display()
        );
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("create {}", dir.display()))?;

        let h264_path = dir.join(format!(
            "face_{}_{}.h264",
            sanitize_path_component(stream_id),
            now
        ));
        let image_path = dir.join(format!(
            "face_{}_{}.jpg",
            sanitize_path_component(stream_id),
            now
        ));
        let data = prepend_h264_config(stream_manager, stream_id, frame);
        debug!(
            "[Analysis][FaceDetection] write h264 frame stream='{}' ts={} h264='{}' bytes={}",
            stream_id,
            frame.timestamp,
            h264_path.display(),
            data.len()
        );
        tokio::fs::write(&h264_path, data)
            .await
            .with_context(|| format!("write {}", h264_path.display()))?;

        debug!(
            "[Analysis][FaceDetection] run ffmpeg stream='{}' ts={} ffmpeg='{}' input='{}' output='{}'",
            stream_id,
            frame.timestamp,
            self.ffmpeg_path,
            h264_path.display(),
            image_path.display()
        );
        let output = Command::new(&self.ffmpeg_path)
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .arg("-f")
            .arg("h264")
            .arg("-i")
            .arg(&h264_path)
            .arg("-frames:v")
            .arg("1")
            .arg("-q:v")
            .arg("2")
            .arg(&image_path)
            .output()
            .await
            .with_context(|| format!("run {}", self.ffmpeg_path))?;
        let _ = tokio::fs::remove_file(&h264_path).await;
        debug!(
            "[Analysis][FaceDetection] ffmpeg finished stream='{}' ts={} status={} stdout='{}' stderr='{}'",
            stream_id,
            frame.timestamp,
            output.status,
            log_snippet(&output.stdout),
            log_snippet(&output.stderr)
        );
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "ffmpeg face frame extract failed: {}",
                stderr.trim()
            ));
        }
        let bytes = tokio::fs::metadata(&image_path)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or_default();
        debug!(
            "[Analysis][FaceDetection] image ready stream='{}' ts={} image='{}' bytes={}",
            stream_id,
            frame.timestamp,
            image_path.display(),
            bytes
        );
        Ok(image_path)
    }
}

struct VisionRequest<'a> {
    stream_id: &'a str,
    frame_timestamp: u64,
    image_path: &'a Path,
}

#[async_trait]
trait VisionBackend: Send + Sync {
    fn name(&self) -> &'static str;

    async fn detect_faces(&self, request: VisionRequest<'_>) -> Result<Vec<FaceBox>>;
}

struct ExternalCommandVisionBackend {
    command: String,
}

#[async_trait]
impl VisionBackend for ExternalCommandVisionBackend {
    fn name(&self) -> &'static str {
        "external_command"
    }

    async fn detect_faces(&self, request: VisionRequest<'_>) -> Result<Vec<FaceBox>> {
        debug!(
            "[Analysis][FaceDetection] run backend stream='{}' ts={} backend='{}' command='{}' image='{}'",
            request.stream_id,
            request.frame_timestamp,
            self.name(),
            self.command,
            request.image_path.display()
        );
        let output = Command::new(&self.command)
            .arg(request.image_path)
            .output()
            .await
            .with_context(|| format!("run face detector {}", self.command))?;
        debug!(
            "[Analysis][FaceDetection] backend finished stream='{}' ts={} backend='{}' status={} stdout='{}' stderr='{}'",
            request.stream_id,
            request.frame_timestamp,
            self.name(),
            output.status,
            log_snippet(&output.stdout),
            log_snippet(&output.stderr)
        );
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("face detector failed: {}", stderr.trim()));
        }
        parse_face_detector_stdout(&output.stdout)
    }
}

fn parse_face_detector_stdout(stdout: &[u8]) -> Result<Vec<FaceBox>> {
    if stdout.is_empty() {
        debug!("[Analysis][FaceDetection] detector stdout empty");
        return Ok(Vec::new());
    }
    if let Ok(output) = serde_json::from_slice::<FaceDetectionOutput>(stdout) {
        debug!(
            "[Analysis][FaceDetection] parsed detector object output face_count={}",
            output.faces.len()
        );
        return Ok(output.faces);
    }
    let faces = serde_json::from_slice::<Vec<FaceBox>>(stdout)
        .with_context(|| format!("parse face detector JSON stdout='{}'", log_snippet(stdout)))?;
    debug!(
        "[Analysis][FaceDetection] parsed detector array output face_count={}",
        faces.len()
    );
    Ok(faces)
}

fn log_snippet(bytes: &[u8]) -> String {
    const MAX_LOG_CHARS: usize = 512;
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    let char_count = text.chars().count();
    if char_count <= MAX_LOG_CHARS {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(MAX_LOG_CHARS).collect();
        format!("{}...(truncated {} chars)", truncated, char_count)
    }
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "stream".to_string()
    } else {
        sanitized
    }
}

struct MetricsState {
    metrics: AnalysisMetrics,
    first_video: Option<MediaFrame>,
    last_video: Option<MediaFrame>,
    last_audio: Option<MediaFrame>,
    frames_since_keyframe: u64,
    gop_sum: u64,
    gop_count: u64,
}

impl MetricsState {
    fn new(stream_id: &str, started_at_ms: u64) -> Self {
        Self {
            metrics: AnalysisMetrics {
                stream_id: stream_id.to_string(),
                started_at_ms,
                updated_at_ms: started_at_ms,
                ..Default::default()
            },
            first_video: None,
            last_video: None,
            last_audio: None,
            frames_since_keyframe: 0,
            gop_sum: 0,
            gop_count: 0,
        }
    }

    fn observe_skipped(&mut self, frame: &MediaFrame) {
        self.metrics.total_frames += 1;
        self.metrics.bytes += frame.data.len() as u64;
    }

    fn observe(&mut self, frame: &MediaFrame) -> Option<AnalysisEvent> {
        self.metrics.total_frames += 1;
        self.metrics.bytes += frame.data.len() as u64;
        self.metrics.updated_at_ms = now_ms();

        match frame.codec {
            CodecType::H264 | CodecType::H265 => self.observe_video(frame),
            CodecType::AAC | CodecType::Opus | CodecType::G711 => self.observe_audio(frame),
            CodecType::Unknown => None,
        }
    }

    fn observe_video(&mut self, frame: &MediaFrame) -> Option<AnalysisEvent> {
        self.metrics.video_frames += 1;
        self.metrics.last_video_timestamp = Some(frame.timestamp);
        self.metrics.last_video_codec = Some(format!("{:?}", frame.codec));
        if self.first_video.is_none() {
            self.first_video = Some(frame.clone());
        }

        if let (Some(first), Some(last)) = (&self.first_video, &self.last_video) {
            let duration_ms = media_frame_timestamp_delta_ms(first, last).max(1);
            self.metrics.video_fps = self.metrics.video_frames as f64 * 1000.0 / duration_ms as f64;
        }
        self.update_bitrate();

        self.frames_since_keyframe += 1;
        let event = if frame.is_keyframe {
            self.metrics.keyframes += 1;
            if self.frames_since_keyframe > 1 {
                self.metrics.last_gop_frames = self.frames_since_keyframe;
                self.gop_sum += self.frames_since_keyframe;
                self.gop_count += 1;
                self.metrics.avg_gop_frames = self.gop_sum as f64 / self.gop_count as f64;
            }
            self.frames_since_keyframe = 0;
            Some(AnalysisEvent {
                stream_id: self.metrics.stream_id.clone(),
                timestamp_ms: now_ms(),
                kind: "keyframe".to_string(),
                message: format!("video keyframe codec={:?}", frame.codec),
                frame_timestamp: frame.timestamp,
                data: None,
            })
        } else {
            None
        };

        self.last_video = Some(frame.clone());
        event
    }

    fn observe_audio(&mut self, frame: &MediaFrame) -> Option<AnalysisEvent> {
        self.metrics.audio_frames += 1;
        self.metrics.last_audio_timestamp = Some(frame.timestamp);
        self.metrics.last_audio_codec = Some(format!("{:?}", frame.codec));
        if let Some(first) = &self.last_audio {
            let duration_ms = media_frame_timestamp_delta_ms(first, frame).max(1);
            self.metrics.audio_fps = 1000.0 / duration_ms as f64;
        }
        self.last_audio = Some(frame.clone());
        self.update_bitrate();
        None
    }

    fn update_bitrate(&mut self) {
        let elapsed_ms = self
            .metrics
            .updated_at_ms
            .saturating_sub(self.metrics.started_at_ms)
            .max(1);
        self.metrics.bitrate_bps = self.metrics.bytes.saturating_mul(8_000) / elapsed_ms;
    }

    fn snapshot(&self) -> AnalysisMetrics {
        self.metrics.clone()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{StreamProtocol, StreamSourceMode};
    use bytes::Bytes;

    #[test]
    fn metrics_records_keyframe_event() {
        let mut state = MetricsState::new("s", now_ms());
        let frame = MediaFrame::new(
            "s".to_string(),
            0,
            0,
            Bytes::from_static(&[0, 0, 0, 1, 0x65]),
            true,
            CodecType::H264,
        )
        .with_clock_rate(crate::core::VIDEO_RTP_CLOCK_RATE);

        let event = state.observe(&frame).expect("keyframe event");
        assert_eq!(event.kind, "keyframe");
        assert_eq!(state.snapshot().keyframes, 1);
    }

    #[tokio::test]
    async fn l1_metrics_plugin_counts_unsampled_frames_without_events() {
        let mut plugin = L1MetricsPlugin::new("s", now_ms(), 2);
        let context = AnalysisPluginContext {
            stream_manager: Arc::new(StreamManager::new()),
            stream_id: "s".to_string(),
        };
        let frame = MediaFrame::new(
            "s".to_string(),
            0,
            0,
            Bytes::from_static(&[0, 0, 0, 1, 0x65]),
            true,
            CodecType::H264,
        )
        .with_clock_rate(crate::core::VIDEO_RTP_CLOCK_RATE);

        let output = plugin
            .process_frame(&context, &frame)
            .await
            .expect("first sampled frame");
        assert_eq!(output.events.len(), 1);
        assert_eq!(output.metrics.expect("metrics").keyframes, 1);

        let output = plugin
            .process_frame(&context, &frame)
            .await
            .expect("second unsampled frame");
        assert!(output.events.is_empty());
        let metrics = output.metrics.expect("metrics");
        assert_eq!(metrics.total_frames, 2);
        assert_eq!(metrics.keyframes, 1);
    }

    #[test]
    fn parses_face_detector_json_output() {
        let faces = parse_face_detector_stdout(
            br#"{"faces":[{"x":10,"y":20,"width":30,"height":40,"confidence":0.95}]}"#,
        )
        .expect("parse object output");
        assert_eq!(faces.len(), 1);
        assert_eq!(faces[0].x, 10);
        assert_eq!(faces[0].confidence, Some(0.95));

        let faces = parse_face_detector_stdout(br#"[{"x":1,"y":2,"width":3,"height":4}]"#)
            .expect("parse array output");
        assert_eq!(faces[0].width, 3);
        assert_eq!(faces[0].confidence, None);
    }

    #[test]
    fn log_snippet_truncates_non_ascii_safely() {
        let input = "检测失败".repeat(200);
        let snippet = log_snippet(input.as_bytes());
        assert!(snippet.contains("检测失败"));
        assert!(snippet.contains("truncated"));
    }

    #[test]
    fn face_detection_requires_detector_command() {
        let stream_manager = Arc::new(StreamManager::new());
        stream_manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::WebRTC, None);
        let manager = AnalysisManager::new(
            stream_manager,
            AnalysisConfig {
                enabled: true,
                face_detector_command: None,
                ..AnalysisConfig::default()
            },
        );

        let err = manager
            .start(StartAnalysisRequest {
                stream_id: "s".to_string(),
                plugins: Some(vec![FACE_DETECTION_PLUGIN.to_string()]),
                sample_interval: None,
            })
            .expect_err("face detection should require detector command");
        assert!(err
            .to_string()
            .contains("face_detection plugin requires face_detector_command"));
    }

    #[test]
    fn unknown_analysis_plugin_is_rejected() {
        let stream_manager = Arc::new(StreamManager::new());
        stream_manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::WebRTC, None);
        let manager = AnalysisManager::new(
            stream_manager,
            AnalysisConfig {
                enabled: true,
                ..AnalysisConfig::default()
            },
        );

        let err = manager
            .start(StartAnalysisRequest {
                stream_id: "s".to_string(),
                plugins: Some(vec!["unknown_plugin".to_string()]),
                sample_interval: None,
            })
            .expect_err("unknown plugin should be rejected");
        assert!(err
            .to_string()
            .contains("unsupported analysis plugin: unknown_plugin"));
    }

    #[tokio::test]
    async fn stop_clears_stream_metrics_and_events() {
        let stream_manager = Arc::new(StreamManager::new());
        stream_manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::WebRTC, None);
        let manager = AnalysisManager::new(
            stream_manager,
            AnalysisConfig {
                enabled: true,
                ..AnalysisConfig::default()
            },
        );

        manager
            .start(StartAnalysisRequest {
                stream_id: "s".to_string(),
                plugins: Some(vec!["l1_metrics".to_string()]),
                sample_interval: None,
            })
            .expect("start analysis");
        manager.metrics.write().insert(
            "s".to_string(),
            AnalysisMetrics {
                stream_id: "s".to_string(),
                ..AnalysisMetrics::default()
            },
        );
        manager.events.write().insert(
            "s".to_string(),
            VecDeque::from([AnalysisEvent {
                stream_id: "s".to_string(),
                timestamp_ms: 1,
                kind: "keyframe".to_string(),
                message: "test".to_string(),
                frame_timestamp: 1,
                data: None,
            }]),
        );

        manager
            .stop(StopAnalysisRequest {
                stream_id: "s".to_string(),
            })
            .expect("stop analysis");

        assert!(manager.metrics("s").is_none());
        assert!(manager.events("s").is_empty());
    }
}
