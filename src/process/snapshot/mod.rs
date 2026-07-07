use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use tokio::time::{timeout, Instant};
use tracing::{error, info, warn};

use crate::core::live_play::{is_idr_frame, is_playable_video_frame};
use crate::core::{
    ensure_annex_b, prepend_annex_b_config_if_needed, CodecType, MediaFrame, StreamManager,
    DEFAULT_SNAPSHOT_DIR,
};
use crate::server::webrtc::request_publisher_keyframe;

const DEFAULT_WAIT_KEYFRAME_MS: u64 = 5_000;
const DEFAULT_FFMPEG_TIMEOUT_SECS: u64 = 10;
const DEFAULT_CAPTURE_JOB_TIMEOUT_SECS: u64 = 30;
const MAX_CONCURRENT_FFMPEG: usize = 2;
const SNAPSHOT_RING_LOOKBACK: u64 = 600;

#[derive(Debug, Clone)]
pub struct SnapshotConfig {
    pub enabled: bool,
    pub base_dir: PathBuf,
    pub ffmpeg_path: String,
    pub wait_keyframe: Duration,
    pub ffmpeg_timeout: Duration,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_dir: PathBuf::from(DEFAULT_SNAPSHOT_DIR),
            ffmpeg_path: "ffmpeg".to_string(),
            wait_keyframe: Duration::from_millis(DEFAULT_WAIT_KEYFRAME_MS),
            ffmpeg_timeout: Duration::from_secs(DEFAULT_FFMPEG_TIMEOUT_SECS),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaptureSnapshotRequest {
    pub stream_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotEntry {
    pub id: String,
    pub stream_id: String,
    pub created_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub source_timestamp: Option<u64>,
    pub path: Option<String>,
    pub url: Option<String>,
    pub format: String,
    pub bytes: u64,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct SnapshotManager {
    stream_manager: Arc<StreamManager>,
    config: SnapshotConfig,
    index: Arc<RwLock<Vec<SnapshotEntry>>>,
    ffmpeg_slots: Arc<Semaphore>,
}

impl SnapshotManager {
    pub fn new(stream_manager: Arc<StreamManager>, config: SnapshotConfig) -> Self {
        Self {
            stream_manager,
            config,
            index: Arc::new(RwLock::new(Vec::new())),
            ffmpeg_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_FFMPEG)),
        }
    }

    pub fn list(&self, stream_id: Option<&str>) -> Vec<SnapshotEntry> {
        self.index
            .read()
            .iter()
            .filter(|entry| stream_id.map(|id| entry.stream_id == id).unwrap_or(true))
            .cloned()
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<SnapshotEntry> {
        self.index
            .read()
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
    }

    pub fn submit(&self, req: CaptureSnapshotRequest) -> Result<SnapshotEntry> {
        if !self.config.enabled {
            return Err(anyhow!("snapshot is disabled"));
        }
        if req.stream_id.trim().is_empty() {
            return Err(anyhow!("missing stream_id"));
        }
        if self.stream_manager.get_stream(&req.stream_id).is_none() {
            return Err(anyhow!("stream not found"));
        }

        let now = now_ms();
        let id = format!("snap_{}_{}", req.stream_id, now);
        let entry = SnapshotEntry {
            id: id.clone(),
            stream_id: req.stream_id.clone(),
            created_at_ms: now,
            completed_at_ms: None,
            source_timestamp: None,
            path: None,
            url: None,
            format: "jpeg".to_string(),
            bytes: 0,
            status: "pending".to_string(),
            error: None,
        };
        self.upsert(entry.clone());

        let manager = self.clone();
        tokio::spawn(async move {
            if let Err(err) = manager.capture_job(id.clone(), req).await {
                error!("[Snapshot] capture failed id='{}': {}", id, err);
                manager.mark_failed(&id, err.to_string());
            }
        });

        Ok(entry)
    }

    async fn capture_job(&self, id: String, req: CaptureSnapshotRequest) -> Result<()> {
        let job_timeout = Duration::from_secs(DEFAULT_CAPTURE_JOB_TIMEOUT_SECS);
        match timeout(job_timeout, self.capture_job_inner(id.clone(), req)).await {
            Ok(result) => result,
            Err(_) => Err(anyhow!(
                "snapshot job timed out after {}s",
                job_timeout.as_secs()
            )),
        }
    }

    async fn capture_job_inner(&self, id: String, req: CaptureSnapshotRequest) -> Result<()> {
        self.update_status(&id, "running");
        let frame = self.wait_latest_idr(&req.stream_id).await?;
        if !matches!(frame.codec, CodecType::H264 | CodecType::H265) {
            return Err(anyhow!("snapshot only supports H264/H265 video stream"));
        }
        let input_format = ffmpeg_raw_video_format(frame.codec)?;
        let input_extension = raw_video_extension(frame.codec)?;
        let prepared = prepare_video_snapshot_frame(&self.stream_manager, &req.stream_id, frame);
        let now = now_ms();
        let dir = self
            .config
            .base_dir
            .join(sanitize_path_component(&req.stream_id))
            .join(date_dir_yyyymmdd(now));
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("create {}", dir.display()))?;

        let raw_path = dir.join(format!("{id}.{input_extension}"));
        let image_path = dir.join(format!("{id}.jpg"));
        tokio::fs::write(&raw_path, &prepared.data)
            .await
            .with_context(|| format!("write {}", raw_path.display()))?;

        let _permit = self
            .ffmpeg_slots
            .acquire()
            .await
            .map_err(|_| anyhow!("snapshot ffmpeg pool closed"))?;

        let ffmpeg_path = self.config.ffmpeg_path.clone();
        let ffmpeg_timeout = self.config.ffmpeg_timeout;
        let raw_for_ffmpeg = raw_path.clone();
        let image_for_ffmpeg = image_path.clone();
        let input_format_owned = input_format.to_string();
        tokio::task::spawn_blocking(move || {
            run_ffmpeg_snapshot_blocking(
                ffmpeg_path,
                input_format_owned,
                raw_for_ffmpeg,
                image_for_ffmpeg,
                ffmpeg_timeout,
            )
        })
        .await
        .context("ffmpeg worker join failed")??;

        let _ = tokio::fs::remove_file(&raw_path).await;

        let bytes = tokio::fs::metadata(&image_path)
            .await
            .map(|m| m.len())
            .unwrap_or_default();
        if bytes == 0 {
            return Err(anyhow!("snapshot output is empty"));
        }

        self.mark_completed(&id, prepared.timestamp, image_path, bytes);
        info!(
            "[Snapshot] captured stream='{}' id='{}' ts={} bytes={}",
            req.stream_id, id, prepared.timestamp, bytes
        );
        Ok(())
    }

    async fn wait_latest_idr(&self, stream_id: &str) -> Result<MediaFrame> {
        if let Some(frame) = self.find_snapshot_frame(stream_id) {
            return Ok(frame);
        }

        let Some(hub) = self.stream_manager.get_hub(stream_id) else {
            return Err(anyhow!("stream hub not found"));
        };
        let mut notify = hub.subscribe_notify();
        let _baseline = *notify.borrow();

        let requested = request_publisher_keyframe(stream_id);
        info!(
            "[Snapshot] waiting fresh IDR stream='{}' requested_keyframe={}",
            stream_id, requested
        );

        let deadline = Instant::now() + self.config.wait_keyframe;
        while Instant::now() < deadline {
            if let Some(frame) = self.find_snapshot_frame(stream_id) {
                return Ok(frame);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let wait_for = remaining.min(Duration::from_millis(200));
            let _ = timeout(wait_for, notify.changed()).await;
        }

        if let Some(frame) = self.find_snapshot_frame(stream_id) {
            return Ok(frame);
        }
        Err(anyhow!("no playable H264/H265 keyframe available"))
    }

    fn find_snapshot_frame(&self, stream_id: &str) -> Option<MediaFrame> {
        let hub = self.stream_manager.get_hub(stream_id)?;
        if let Some(frame) = hub.latest_idr_frame() {
            if Self::is_snapshot_candidate(&frame) {
                return Some(frame);
            }
        }

        let latest = hub.latest_seq();
        if hub.is_empty() {
            return None;
        }
        let oldest = hub.oldest_seq().unwrap_or(0);
        let from = latest.saturating_sub(SNAPSHOT_RING_LOOKBACK).max(oldest);
        hub.frames_from(from, latest)
            .into_iter()
            .rev()
            .find(|frame| Self::is_snapshot_candidate(frame) && is_idr_frame(frame))
    }

    fn is_snapshot_candidate(frame: &MediaFrame) -> bool {
        is_playable_video_frame(frame) && !frame.data.is_empty()
    }
    fn upsert(&self, entry: SnapshotEntry) {
        let mut index = self.index.write();
        if let Some(existing) = index.iter_mut().find(|existing| existing.id == entry.id) {
            *existing = entry;
        } else {
            index.push(entry);
        }
    }

    fn update_status(&self, id: &str, status: &str) {
        if let Some(entry) = self.index.write().iter_mut().find(|entry| entry.id == id) {
            entry.status = status.to_string();
        }
    }

    fn mark_completed(&self, id: &str, source_timestamp: u64, image_path: PathBuf, bytes: u64) {
        let mut index = self.index.write();
        if let Some(entry) = index.iter_mut().find(|entry| entry.id == id) {
            entry.completed_at_ms = Some(now_ms());
            entry.source_timestamp = Some(source_timestamp);
            entry.path = Some(image_path.to_string_lossy().to_string());
            entry.url = Some(format!("/api/snapshots/{id}.jpg"));
            entry.bytes = bytes;
            entry.status = "completed".to_string();
            entry.error = None;
        }
    }

    fn mark_failed(&self, id: &str, error: String) {
        let mut index = self.index.write();
        if let Some(entry) = index.iter_mut().find(|entry| entry.id == id) {
            entry.completed_at_ms = Some(now_ms());
            entry.status = "error".to_string();
            entry.error = Some(error);
        }
    }
}

fn run_ffmpeg_snapshot_blocking(
    ffmpeg_path: String,
    input_format: String,
    raw_path: PathBuf,
    image_path: PathBuf,
    timeout_budget: Duration,
) -> Result<()> {
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::Instant as StdInstant;

    let mut child = Command::new(&ffmpeg_path)
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-f")
        .arg(&input_format)
        .arg("-i")
        .arg(&raw_path)
        .arg("-frames:v")
        .arg("1")
        .arg("-q:v")
        .arg("2")
        .arg("-strict")
        .arg("-2")
        .arg(&image_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {ffmpeg_path}"))?;

    let started = StdInstant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(anyhow!("ffmpeg snapshot failed: exit {status}"));
            }
            Ok(None) => {
                if started.elapsed() >= timeout_budget {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(anyhow!(
                        "ffmpeg snapshot timed out after {}s",
                        timeout_budget.as_secs()
                    ));
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(anyhow!("ffmpeg wait error: {e}")),
        }
    }
}

fn ffmpeg_raw_video_format(codec: CodecType) -> Result<&'static str> {
    match codec {
        CodecType::H264 => Ok("h264"),
        CodecType::H265 => Ok("hevc"),
        _ => Err(anyhow!("unsupported snapshot video codec: {:?}", codec)),
    }
}

fn raw_video_extension(codec: CodecType) -> Result<&'static str> {
    match codec {
        CodecType::H264 => Ok("h264"),
        CodecType::H265 => Ok("h265"),
        _ => Err(anyhow!("unsupported snapshot video codec: {:?}", codec)),
    }
}

fn prepare_video_snapshot_frame(
    manager: &StreamManager,
    stream_id: &str,
    frame: MediaFrame,
) -> MediaFrame {
    let au = ensure_annex_b(&frame.data);
    let data = manager
        .get_stream(&stream_id.to_string())
        .map(|stream| {
            prepend_annex_b_config_if_needed(
                frame.codec,
                stream.vps.as_deref(),
                stream.sps.as_deref(),
                stream.pps.as_deref(),
                &au,
            )
        })
        .unwrap_or(au);
    let prepared = MediaFrame::new(
        frame.stream_id,
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
    date_dir_yyyymmdd_utc(timestamp_ms)
}

fn date_dir_yyyymmdd_utc(timestamp_ms: u64) -> String {
    let dt = OffsetDateTime::from_unix_timestamp((timestamp_ms / 1_000) as i64)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    format!("{:04}{:02}{:02}", dt.year(), dt.month() as u8, dt.day())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{StreamProtocol, StreamSourceMode, VIDEO_RTP_CLOCK_RATE};

    #[test]
    fn sanitizes_stream_id_for_path() {
        assert_eq!(sanitize_path_component("cam/a:b"), "cam_a_b");
    }

    #[test]
    fn date_dir_uses_yyyymmdd_not_epoch_day() {
        assert_eq!(date_dir_yyyymmdd_utc(0), "19700101");
        assert_eq!(date_dir_yyyymmdd_utc(1_782_921_600_000), "20260701");
    }

    #[test]
    fn h264_snapshot_frame_prepends_sps_pps() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::WebRTC, None);
        manager.set_stream_sps_pps("s", vec![0x67, 0x42], vec![0x68, 0xce]);
        let frame = MediaFrame::new(
            "s".to_string(),
            0,
            90_000,
            Bytes::from_static(&[0, 0, 0, 1, 0x65, 0x88]),
            true,
            CodecType::H264,
        )
        .with_clock_rate(VIDEO_RTP_CLOCK_RATE);

        let prepared = prepare_video_snapshot_frame(&manager, "s", frame);

        assert!(prepared.data.windows(5).any(|w| w == [0, 0, 0, 1, 0x67]));
        assert!(prepared.data.windows(5).any(|w| w == [0, 0, 0, 1, 0x68]));
        assert!(prepared.data.windows(5).any(|w| w == [0, 0, 0, 1, 0x65]));
        assert_eq!(prepared.clock_rate, Some(VIDEO_RTP_CLOCK_RATE));
    }

    #[test]
    fn h265_snapshot_frame_prepends_vps_sps_pps() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::WebRTC, None);
        manager.set_stream_hevc_params(
            "s",
            vec![0x40, 0x01, 0x0c, 0x01],
            vec![0x42, 0x01, 0x01, 0x60],
            vec![0x44, 0x01, 0xc0, 0xf1],
        );
        let frame = MediaFrame::new(
            "s".to_string(),
            0,
            90_000,
            Bytes::from_static(&[0, 0, 0, 1, 0x26, 0x01, 0xaa]),
            true,
            CodecType::H265,
        )
        .with_clock_rate(VIDEO_RTP_CLOCK_RATE);

        let prepared = prepare_video_snapshot_frame(&manager, "s", frame);

        assert!(prepared
            .data
            .windows(8)
            .any(|w| w == [0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01]));
        assert!(prepared
            .data
            .windows(8)
            .any(|w| w == [0, 0, 0, 1, 0x42, 0x01, 0x01, 0x60]));
        assert!(prepared
            .data
            .windows(8)
            .any(|w| w == [0, 0, 0, 1, 0x44, 0x01, 0xc0, 0xf1]));
        assert!(prepared
            .data
            .windows(7)
            .any(|w| w == [0, 0, 0, 1, 0x26, 0x01, 0xaa]));
        assert_eq!(prepared.codec, CodecType::H265);
        assert_eq!(prepared.clock_rate, Some(VIDEO_RTP_CLOCK_RATE));
    }

    #[test]
    fn h265_snapshot_uses_hevc_raw_demuxer() {
        assert_eq!(ffmpeg_raw_video_format(CodecType::H264).unwrap(), "h264");
        assert_eq!(raw_video_extension(CodecType::H264).unwrap(), "h264");
        assert_eq!(ffmpeg_raw_video_format(CodecType::H265).unwrap(), "hevc");
        assert_eq!(raw_video_extension(CodecType::H265).unwrap(), "h265");
    }

    #[tokio::test]
    async fn submit_snapshot_returns_pending_entry() {
        let stream_manager = Arc::new(StreamManager::new());
        stream_manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::WebRTC, None);
        let manager = SnapshotManager::new(
            stream_manager,
            SnapshotConfig {
                enabled: true,
                wait_keyframe: Duration::from_millis(1),
                ..SnapshotConfig::default()
            },
        );

        let entry = manager
            .submit(CaptureSnapshotRequest {
                stream_id: "s".to_string(),
            })
            .expect("submit snapshot");

        assert_eq!(entry.status, "pending");
        assert!(entry.path.is_none());
        assert!(manager.get(&entry.id).is_some());
    }
}
