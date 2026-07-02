use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use time::{OffsetDateTime, UtcOffset};
use tokio::process::Command;
use tokio::time::{sleep, Instant};
use tracing::{error, info, warn};

use crate::core::live_play::{is_idr_frame, is_playable_video_frame, prepend_h264_config};
use crate::core::{CodecType, MediaFrame, StreamManager};
use crate::webrtc::request_publisher_keyframe;

const DEFAULT_WAIT_KEYFRAME_MS: u64 = 1_000;

#[derive(Debug, Clone)]
pub struct SnapshotConfig {
    pub enabled: bool,
    pub base_dir: PathBuf,
    pub ffmpeg_path: String,
    pub wait_keyframe: Duration,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_dir: PathBuf::from("./snapshots"),
            ffmpeg_path: "ffmpeg".to_string(),
            wait_keyframe: Duration::from_millis(DEFAULT_WAIT_KEYFRAME_MS),
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
}

impl SnapshotManager {
    pub fn new(stream_manager: Arc<StreamManager>, config: SnapshotConfig) -> Self {
        Self {
            stream_manager,
            config,
            index: Arc::new(RwLock::new(Vec::new())),
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
        self.update_status(&id, "running");
        let frame = self.wait_latest_idr(&req.stream_id).await?;
        if frame.codec != CodecType::H264 {
            return Err(anyhow!("snapshot only supports H264 stream for now"));
        }
        let prepared = prepare_h264_snapshot_frame(&self.stream_manager, &req.stream_id, frame);
        let now = now_ms();
        let dir = self
            .config
            .base_dir
            .join(sanitize_path_component(&req.stream_id))
            .join(date_dir_yyyymmdd(now));
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("create {}", dir.display()))?;

        let h264_path = dir.join(format!("{id}.h264"));
        let image_path = dir.join(format!("{id}.jpg"));
        tokio::fs::write(&h264_path, &prepared.data)
            .await
            .with_context(|| format!("write {}", h264_path.display()))?;

        let output = Command::new(&self.config.ffmpeg_path)
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
            .with_context(|| format!("run {}", self.config.ffmpeg_path))?;
        let _ = tokio::fs::remove_file(&h264_path).await;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("ffmpeg snapshot failed: {}", stderr.trim()));
        }

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
        if let Some(frame) = self.latest_snapshot_frame(stream_id) {
            return Ok(frame);
        }

        let requested = request_publisher_keyframe(stream_id);
        info!(
            "[Snapshot] waiting fresh IDR stream='{}' requested_keyframe={}",
            stream_id, requested
        );
        let deadline = Instant::now() + self.config.wait_keyframe;
        while Instant::now() < deadline {
            if let Some(frame) = self.latest_snapshot_frame(stream_id) {
                return Ok(frame);
            }
            sleep(Duration::from_millis(50)).await;
        }
        Err(anyhow!("no playable H264 keyframe available"))
    }

    fn latest_snapshot_frame(&self, stream_id: &str) -> Option<MediaFrame> {
        let hub = self.stream_manager.get_hub(stream_id)?;
        let frame = hub.latest_idr_frame()?;
        if frame.codec != CodecType::H264 {
            warn!(
                "[Snapshot] unsupported codec stream='{}' codec={:?}",
                stream_id, frame.codec
            );
            return Some(frame);
        }
        if is_playable_video_frame(&frame) && is_idr_frame(&frame) {
            Some(frame)
        } else {
            None
        }
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
            entry.status = "failed".to_string();
            entry.error = Some(error);
        }
    }
}

fn prepare_h264_snapshot_frame(
    manager: &StreamManager,
    stream_id: &str,
    frame: MediaFrame,
) -> MediaFrame {
    let data = prepend_h264_config(manager, stream_id, &frame);
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
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    date_dir_yyyymmdd_with_offset(timestamp_ms, offset)
}

fn date_dir_yyyymmdd_with_offset(timestamp_ms: u64, offset: UtcOffset) -> String {
    let dt = OffsetDateTime::from_unix_timestamp((timestamp_ms / 1_000) as i64)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .to_offset(offset);
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
        assert_eq!(date_dir_yyyymmdd_with_offset(0, UtcOffset::UTC), "19700101");
        assert_eq!(
            date_dir_yyyymmdd_with_offset(1_782_921_600_000, UtcOffset::from_hms(8, 0, 0).unwrap()),
            "20260702"
        );
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

        let prepared = prepare_h264_snapshot_frame(&manager, "s", frame);

        assert!(prepared.data.windows(5).any(|w| w == [0, 0, 0, 1, 0x67]));
        assert!(prepared.data.windows(5).any(|w| w == [0, 0, 0, 1, 0x68]));
        assert!(prepared.data.windows(5).any(|w| w == [0, 0, 0, 1, 0x65]));
        assert_eq!(prepared.clock_rate, Some(VIDEO_RTP_CLOCK_RATE));
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
