use anyhow::{anyhow, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{error, info};

use crate::core::{
    media_frame_timestamp_delta_ms, CodecType, DispatchError, DispatchPolicy, MediaFrame,
    StreamManager,
};

const MAX_EVENTS_PER_STREAM: usize = 256;

#[derive(Debug, Clone)]
pub struct AnalysisConfig {
    pub enabled: bool,
    pub default_sample_interval: u64,
    pub max_events_per_stream: usize,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_sample_interval: 1,
            max_events_per_stream: MAX_EVENTS_PER_STREAM,
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

        let task = AnalysisTask {
            stream_manager: self.stream_manager.clone(),
            info: info.clone(),
            metrics: self.metrics.clone(),
            events: self.events.clone(),
            max_events: self.config.max_events_per_stream,
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
        Ok(session.info)
    }
}

struct AnalysisTask {
    stream_manager: Arc<StreamManager>,
    info: AnalysisSessionInfo,
    metrics: Arc<RwLock<HashMap<String, AnalysisMetrics>>>,
    events: Arc<RwLock<HashMap<String, VecDeque<AnalysisEvent>>>>,
    max_events: usize,
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

        let mut state = MetricsState::new(&self.info.stream_id, self.info.started_at_ms);
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
                });
            }
            for frame in frames {
                if state.metrics.total_frames % self.info.sample_interval != 0 {
                    state.observe_skipped(&frame);
                    continue;
                }
                if let Some(event) = state.observe(&frame) {
                    self.push_event(event);
                }
                self.metrics
                    .write()
                    .insert(self.info.stream_id.clone(), state.snapshot());
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
}
