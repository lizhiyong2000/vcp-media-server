use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use time::{OffsetDateTime, UtcOffset};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use tracing::{error, info};

use crate::core::live_play::prepend_h264_config;
use crate::core::{
    CodecType, DispatchError, DispatchPolicy, FlvPlayTimeline, MediaFrame, StreamManager,
    MILLISECOND_CLOCK_RATE,
};
use crate::hls::ts_muxer::TsMuxer;

const DEFAULT_SEGMENT_DURATION_SEC: u64 = 300;
const ACTIVE_INDEX_FLUSH_INTERVAL_MS: u64 = 5_000;

#[derive(Debug, Clone)]
pub struct RecordConfig {
    pub enabled: bool,
    pub base_dir: PathBuf,
    pub default_format: RecordFormat,
    pub segment_duration: Duration,
    pub align_keyframe: bool,
}

impl Default for RecordConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_dir: PathBuf::from("./recordings"),
            default_format: RecordFormat::Ts,
            segment_duration: Duration::from_secs(DEFAULT_SEGMENT_DURATION_SEC),
            align_keyframe: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordFormat {
    Ts,
}

impl RecordFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "ts" | "mpegts" => Some(Self::Ts),
            _ => None,
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Ts => "ts",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordingEntry {
    pub id: String,
    pub stream_id: String,
    pub session_id: String,
    pub format: RecordFormat,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub duration_ms: u64,
    pub path: String,
    pub bytes: u64,
    pub video_frames: u64,
    pub audio_frames: u64,
    pub keyframes: u64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordSessionInfo {
    pub session_id: String,
    pub stream_id: String,
    pub format: RecordFormat,
    pub started_at_ms: u64,
    pub segment_duration_ms: u64,
    pub align_keyframe: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartRecordRequest {
    pub stream_id: String,
    pub format: Option<String>,
    pub segment_duration: Option<u64>,
    pub align_keyframe: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StopRecordRequest {
    pub stream_id: Option<String>,
    pub session_id: Option<String>,
}

struct ActiveRecordSession {
    info: RecordSessionInfo,
    stop_tx: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct RecorderManager {
    stream_manager: Arc<StreamManager>,
    config: RecordConfig,
    active: Arc<RwLock<HashMap<String, ActiveRecordSession>>>,
    index: Arc<RwLock<Vec<RecordingEntry>>>,
}

impl RecorderManager {
    pub fn new(stream_manager: Arc<StreamManager>, config: RecordConfig) -> Self {
        Self {
            stream_manager,
            config,
            active: Arc::new(RwLock::new(HashMap::new())),
            index: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn list_recordings(&self, stream_id: Option<&str>) -> Vec<RecordingEntry> {
        self.index
            .read()
            .iter()
            .filter(|entry| stream_id.map(|id| entry.stream_id == id).unwrap_or(true))
            .cloned()
            .collect()
    }

    pub fn active_sessions(&self) -> Vec<RecordSessionInfo> {
        self.active
            .read()
            .values()
            .map(|session| session.info.clone())
            .collect()
    }

    pub fn start(&self, req: StartRecordRequest) -> Result<RecordSessionInfo> {
        if !self.config.enabled {
            return Err(anyhow!("recording is disabled"));
        }
        if req.stream_id.trim().is_empty() {
            return Err(anyhow!("missing stream_id"));
        }
        let stream = self
            .stream_manager
            .get_stream(&req.stream_id)
            .ok_or_else(|| anyhow!("stream not found"))?;
        if self.active.read().contains_key(&req.stream_id) {
            return Err(anyhow!("recording already active for stream"));
        }

        let format = req
            .format
            .as_deref()
            .and_then(RecordFormat::parse)
            .unwrap_or(self.config.default_format);
        let segment_duration = Duration::from_secs(
            req.segment_duration
                .unwrap_or(self.config.segment_duration.as_secs())
                .max(1),
        );
        let align_keyframe = req.align_keyframe.unwrap_or(self.config.align_keyframe);
        let declared_has_video = stream.tracks.is_empty()
            || stream
                .tracks
                .iter()
                .any(|track| matches!(track.codec, CodecType::H264 | CodecType::H265));
        let declared_has_audio = stream.tracks.is_empty()
            || stream
                .tracks
                .iter()
                .any(|track| matches!(track.codec, CodecType::AAC));
        let session_id = format!("rec_{}_{}", req.stream_id, now_ms());
        let info = RecordSessionInfo {
            session_id: session_id.clone(),
            stream_id: req.stream_id.clone(),
            format,
            started_at_ms: now_ms(),
            segment_duration_ms: segment_duration.as_millis() as u64,
            align_keyframe,
        };

        let (stop_tx, stop_rx) = watch::channel(false);
        let task = RecordTask {
            stream_manager: self.stream_manager.clone(),
            stream_id: req.stream_id.clone(),
            session_id,
            format,
            base_dir: self.config.base_dir.clone(),
            segment_duration,
            align_keyframe,
            declared_has_video,
            declared_has_audio,
            index: self.index.clone(),
            stop_rx,
        };
        let _handle = tokio::spawn(async move {
            if let Err(err) = task.run().await {
                error!("[Record] session failed: {err:?}");
            }
        });

        self.active.write().insert(
            req.stream_id,
            ActiveRecordSession {
                info: info.clone(),
                stop_tx,
            },
        );
        Ok(info)
    }

    pub fn stop(&self, req: StopRecordRequest) -> Result<RecordSessionInfo> {
        let key = if let Some(stream_id) = req.stream_id {
            stream_id
        } else if let Some(session_id) = req.session_id {
            self.active
                .read()
                .iter()
                .find(|(_, session)| session.info.session_id == session_id)
                .map(|(stream_id, _)| stream_id.clone())
                .ok_or_else(|| anyhow!("recording session not found"))?
        } else {
            return Err(anyhow!("missing stream_id or session_id"));
        };

        let session = self
            .active
            .write()
            .remove(&key)
            .ok_or_else(|| anyhow!("recording session not found"))?;
        let _ = session.stop_tx.send(true);
        Ok(session.info)
    }
}

struct RecordTask {
    stream_manager: Arc<StreamManager>,
    stream_id: String,
    session_id: String,
    format: RecordFormat,
    base_dir: PathBuf,
    segment_duration: Duration,
    align_keyframe: bool,
    declared_has_video: bool,
    declared_has_audio: bool,
    index: Arc<RwLock<Vec<RecordingEntry>>>,
    stop_rx: watch::Receiver<bool>,
}

impl RecordTask {
    async fn run(mut self) -> Result<()> {
        info!(
            "[Record] start stream='{}' session='{}' format={:?}",
            self.stream_id, self.session_id, self.format
        );
        self.stream_manager.ensure_stream_hub(&self.stream_id);
        let mut reader = self
            .stream_manager
            .dispatch_subscribe(&self.stream_id, DispatchPolicy::SequentialFromIdr)
            .ok_or_else(|| anyhow!("stream hub not available"))?;

        let mut writer = SegmentWriter::new(&self)?;
        writer.persist_index(&self.index, "recording").await?;
        loop {
            let frames = tokio::select! {
                _ = self.stop_rx.changed() => {
                    if *self.stop_rx.borrow() {
                        break;
                    }
                    continue;
                }
                result = reader.recv_batch() => match result {
                    Ok(frames) if !frames.is_empty() => frames,
                    Ok(_) => continue,
                    Err(DispatchError::Closed) => break,
                }
            };
            if reader.take_muxer_resync() {
                writer.finish(&self.index).await?;
                writer = SegmentWriter::new(&self)?;
                writer.persist_index(&self.index, "recording").await?;
            }
            for frame in frames {
                writer.write_frame(&self, frame).await?;
            }
        }
        writer.finish(&self.index).await?;
        Ok(())
    }
}

struct SegmentWriter {
    id: String,
    stream_id: String,
    session_id: String,
    format: RecordFormat,
    path: PathBuf,
    file: tokio::fs::File,
    muxer: TsMuxer,
    timeline: FlvPlayTimeline,
    started_at_ms: u64,
    ended_at_ms: u64,
    has_video: bool,
    has_audio: bool,
    video_frames: u64,
    audio_frames: u64,
    keyframes: u64,
    bytes: u64,
    header_written: bool,
    last_index_flush_ms: u64,
}

impl SegmentWriter {
    fn new(task: &RecordTask) -> Result<Self> {
        let started_at_ms = now_ms();
        let dir = task
            .base_dir
            .join(sanitize_path_component(&task.stream_id))
            .join(date_dir_yyyymmdd(started_at_ms));
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let id = format!("{}_{}", task.session_id, started_at_ms);
        let path = dir.join(format!("{}.{}", id, task.format.extension()));
        let file =
            std::fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
        Ok(Self {
            id,
            stream_id: task.stream_id.clone(),
            session_id: task.session_id.clone(),
            format: task.format,
            path,
            file: tokio::fs::File::from_std(file),
            muxer: TsMuxer::new(),
            timeline: FlvPlayTimeline::default(),
            started_at_ms,
            ended_at_ms: started_at_ms,
            has_video: task.declared_has_video,
            has_audio: task.declared_has_audio,
            video_frames: 0,
            audio_frames: 0,
            keyframes: 0,
            bytes: 0,
            header_written: false,
            last_index_flush_ms: started_at_ms,
        })
    }

    async fn write_frame(&mut self, task: &RecordTask, frame: MediaFrame) -> Result<()> {
        if should_rotate(self, task, &frame) {
            self.finish(&task.index).await?;
            *self = Self::new(task)?;
        }

        let is_video = matches!(frame.codec, CodecType::H264 | CodecType::H265);
        let is_audio = matches!(frame.codec, CodecType::AAC);
        if !is_video && !is_audio {
            return Ok(());
        }

        let frame = if is_video {
            self.has_video = true;
            self.video_frames += 1;
            if frame.is_keyframe {
                self.keyframes += 1;
            }
            prepare_video_frame_for_record(&task.stream_manager, &task.stream_id, frame)
        } else {
            self.has_audio = true;
            self.audio_frames += 1;
            frame
        };

        if !self.header_written {
            let header = self.muxer.generate_pat_pmt(self.has_video, self.has_audio);
            self.write_bytes(&header).await?;
            self.header_written = true;
        }

        let mux_ts_ms = self.timeline.map(&frame) as u64;
        self.muxer.update_pcr(mux_ts_ms);
        let frame = frame
            .with_timestamp(mux_ts_ms)
            .with_clock_rate(MILLISECOND_CLOCK_RATE);
        let data = self.muxer.frame_to_ts(&frame);
        if !data.is_empty() {
            self.write_bytes(&data).await?;
            self.ended_at_ms = now_ms();
            self.flush_active_index_if_due(&task.index).await?;
        }
        Ok(())
    }

    async fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        self.file.write_all(data).await?;
        self.bytes += data.len() as u64;
        Ok(())
    }

    async fn persist_index(
        &mut self,
        index: &Arc<RwLock<Vec<RecordingEntry>>>,
        status: &str,
    ) -> Result<()> {
        let entry = self.build_entry(status);
        let entries = upsert_index_entry(index, entry);
        write_index_file(&self.path, &entries).await?;
        self.last_index_flush_ms = now_ms();
        Ok(())
    }

    async fn flush_active_index_if_due(
        &mut self,
        index: &Arc<RwLock<Vec<RecordingEntry>>>,
    ) -> Result<()> {
        if now_ms().saturating_sub(self.last_index_flush_ms) >= ACTIVE_INDEX_FLUSH_INTERVAL_MS {
            self.persist_index(index, "recording").await?;
        }
        Ok(())
    }

    async fn finish(&mut self, index: &Arc<RwLock<Vec<RecordingEntry>>>) -> Result<()> {
        self.file.flush().await?;
        if self.bytes == 0 || self.video_frames == 0 {
            self.persist_index(index, "empty").await?;
            return Ok(());
        }
        let entry = self.build_entry("completed");
        let entries = upsert_index_entry(index, entry.clone());
        write_index_file(&self.path, &entries).await?;
        info!(
            "[Record] segment complete id={} path={} bytes={} video={} audio={}",
            entry.id, entry.path, entry.bytes, entry.video_frames, entry.audio_frames
        );
        Ok(())
    }

    fn build_entry(&self, status: &str) -> RecordingEntry {
        RecordingEntry {
            id: self.id.clone(),
            stream_id: self.stream_id.clone(),
            session_id: self.session_id.clone(),
            format: self.format,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
            duration_ms: self.ended_at_ms.saturating_sub(self.started_at_ms),
            path: self.path.to_string_lossy().to_string(),
            bytes: self.bytes,
            video_frames: self.video_frames,
            audio_frames: self.audio_frames,
            keyframes: self.keyframes,
            status: status.to_string(),
        }
    }
}

fn should_rotate(writer: &SegmentWriter, task: &RecordTask, frame: &MediaFrame) -> bool {
    if writer.bytes == 0 {
        return false;
    }
    if writer.started_at_ms + task.segment_duration.as_millis() as u64 > now_ms() {
        return false;
    }
    if !task.align_keyframe {
        return true;
    }
    matches!(frame.codec, CodecType::H264 | CodecType::H265) && frame.is_keyframe
}

fn prepare_video_frame_for_record(
    manager: &StreamManager,
    stream_id: &str,
    frame: MediaFrame,
) -> MediaFrame {
    if !matches!(frame.codec, CodecType::H264) {
        return frame;
    }
    let data = prepend_h264_config(manager, stream_id, &frame);
    let prepared = MediaFrame::new(
        frame.stream_id.clone(),
        frame.track_id,
        frame.timestamp,
        Bytes::from(data),
        frame.is_keyframe,
        frame.codec,
    )
    .with_optional_clock_rate(frame.clock_rate);
    if let Some(rtp_data) = frame.rtp_data {
        prepared.with_rtp_data(rtp_data)
    } else {
        prepared
    }
}

fn upsert_index_entry(
    index: &Arc<RwLock<Vec<RecordingEntry>>>,
    entry: RecordingEntry,
) -> Vec<RecordingEntry> {
    let mut index = index.write();
    if let Some(existing) = index.iter_mut().find(|existing| existing.id == entry.id) {
        *existing = entry;
    } else {
        index.push(entry);
    }
    index.clone()
}

async fn write_index_file(segment_path: &Path, entries: &[RecordingEntry]) -> Result<()> {
    let Some(dir) = segment_path.parent() else {
        return Ok(());
    };
    let data = serde_json::to_vec_pretty(entries)?;
    tokio::fs::write(dir.join("index.json"), data).await?;
    Ok(())
}

fn sanitize_path_component(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn date_dir_yyyymmdd(timestamp_ms: u64) -> String {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    date_dir_yyyymmdd_with_offset(timestamp_ms, offset)
}

fn date_dir_yyyymmdd_with_offset(timestamp_ms: u64, offset: UtcOffset) -> String {
    let dt = OffsetDateTime::from_unix_timestamp((timestamp_ms / 1_000) as i64)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .to_offset(offset);
    format!("{:04}{:02}{:02}", dt.year(), dt.month() as u8, dt.day())
}

trait FrameTimestampExt {
    fn with_timestamp(self, timestamp: u64) -> Self;
}

impl FrameTimestampExt for MediaFrame {
    fn with_timestamp(mut self, timestamp: u64) -> Self {
        self.timestamp = timestamp;
        self.data = Bytes::copy_from_slice(&self.data);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{StreamProtocol, StreamSourceMode, VIDEO_RTP_CLOCK_RATE};

    #[test]
    fn parses_ts_format_aliases() {
        assert_eq!(RecordFormat::parse("ts"), Some(RecordFormat::Ts));
        assert_eq!(RecordFormat::parse("mpegts"), Some(RecordFormat::Ts));
        assert_eq!(RecordFormat::parse("mp4"), None);
    }

    #[test]
    fn sanitizes_stream_id_for_path() {
        assert_eq!(sanitize_path_component("a/b:c"), "a_b_c");
    }

    #[test]
    fn date_dir_uses_yyyymmdd_not_epoch_day() {
        assert_eq!(date_dir_yyyymmdd_with_offset(0, UtcOffset::UTC), "19700101");
        assert_eq!(
            date_dir_yyyymmdd_with_offset(1_782_921_600_000, UtcOffset::from_hms(8, 0, 0).unwrap()),
            "20260702"
        );
    }

    #[test]
    fn record_h264_keyframe_prepends_sps_pps() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::WebRTC, None);
        manager.set_stream_sps_pps("s", vec![0x67, 0x42, 0x00, 0x1f], vec![0x68, 0xce]);
        let frame = MediaFrame::new(
            "s".to_string(),
            0,
            90_000,
            Bytes::from_static(&[0, 0, 0, 1, 0x65, 0x88]),
            true,
            CodecType::H264,
        )
        .with_clock_rate(VIDEO_RTP_CLOCK_RATE);

        let prepared = prepare_video_frame_for_record(&manager, "s", frame);

        assert!(prepared.data.windows(5).any(|w| w == [0, 0, 0, 1, 0x67]));
        assert!(prepared.data.windows(5).any(|w| w == [0, 0, 0, 1, 0x68]));
        assert!(prepared.data.windows(5).any(|w| w == [0, 0, 0, 1, 0x65]));
        assert_eq!(prepared.clock_rate, Some(VIDEO_RTP_CLOCK_RATE));
    }

    #[test]
    fn index_upsert_updates_existing_entry() {
        let index = Arc::new(RwLock::new(Vec::new()));
        let mut entry = RecordingEntry {
            id: "seg1".to_string(),
            stream_id: "s".to_string(),
            session_id: "rec1".to_string(),
            format: RecordFormat::Ts,
            started_at_ms: 1,
            ended_at_ms: 1,
            duration_ms: 0,
            path: "seg1.ts".to_string(),
            bytes: 0,
            video_frames: 0,
            audio_frames: 0,
            keyframes: 0,
            status: "recording".to_string(),
        };
        upsert_index_entry(&index, entry.clone());

        entry.bytes = 1024;
        entry.video_frames = 3;
        entry.status = "completed".to_string();
        let entries = upsert_index_entry(&index, entry);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].bytes, 1024);
        assert_eq!(entries[0].status, "completed");
    }
}
