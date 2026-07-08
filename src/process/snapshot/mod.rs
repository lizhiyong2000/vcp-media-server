use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use tokio::time::{Instant, timeout};
use tracing::{debug, error, info};

use crate::core::live_play::{is_idr_frame, is_playable_video_frame};
use crate::core::{
    CodecType, DEFAULT_SNAPSHOT_DIR, MediaFrame, StreamHub, StreamManager, StreamSourceMode,
    ensure_annex_b, iter_annex_b_nal_ranges, prepend_annex_b_config_if_needed,
};
use crate::server::webrtc::request_publisher_keyframe;

const DEFAULT_WAIT_KEYFRAME_MS: u64 = 5_000;
const PULL_WAIT_KEYFRAME_MS: u64 = 120_000;
const DEFAULT_FFMPEG_TIMEOUT_SECS: u64 = 10;
const DEFAULT_CAPTURE_JOB_TIMEOUT_SECS: u64 = 30;
const PULL_CAPTURE_JOB_TIMEOUT_SECS: u64 = 130;
const PULL_KEYFRAME_WAIT_BUFFER: Duration = Duration::from_secs(5);
const MAX_CONCURRENT_FFMPEG: usize = 2;
const SNAPSHOT_MAX_VIDEO_FRAMES: usize = 120;
const SNAPSHOT_MAX_RAW_BYTES: usize = 4 * 1024 * 1024;
const SNAPSHOT_RING_LOOKBACK: u64 = 600;
const SNAPSHOT_H264_INTRA_PREROLL_FRAMES: u64 = 120;
const SNAPSHOT_H264_INTRA_SKIP_DECODED_FRAMES: usize = 30;
const SNAPSHOT_PULL_IDR_MIN_VIDEO_FRAMES: usize = 8;
const SNAPSHOT_PULL_IDR_COLLECT_WAIT_MS: u64 = 1_500;

#[derive(Debug, Clone)]
struct SnapshotSelection {
    frame: MediaFrame,
    seq: u64,
    h264_intra_fallback: bool,
}

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
        let id = format!("snap_{}_{}", snapshot_id_component(&req.stream_id), now);
        debug!(
            "[Snapshot] submit accepted stream='{}' id='{}' base_dir='{}' wait_keyframe_ms={} ffmpeg_timeout_ms={}",
            req.stream_id,
            id,
            self.config.base_dir.display(),
            self.config.wait_keyframe.as_millis(),
            self.config.ffmpeg_timeout.as_millis()
        );
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
        let stream_id = req.stream_id.clone();
        let job_timeout = self.capture_job_timeout_for(&stream_id);
        debug!(
            "[Snapshot] job start stream='{}' id='{}' timeout_ms={}",
            stream_id,
            id,
            job_timeout.as_millis()
        );
        match timeout(job_timeout, self.capture_job_inner(id.clone(), req)).await {
            Ok(result) => result,
            Err(_) => {
                debug!(
                    "[Snapshot] job timeout stream='{}' id='{}' timeout_ms={}",
                    stream_id,
                    id,
                    job_timeout.as_millis()
                );
                Err(anyhow!(
                    "snapshot job timed out after {}s",
                    job_timeout.as_secs()
                ))
            }
        }
    }

    async fn capture_job_inner(&self, id: String, req: CaptureSnapshotRequest) -> Result<()> {
        let started = Instant::now();
        self.update_status(&id, "running");
        debug!(
            "[Snapshot] status running stream='{}' id='{}'",
            req.stream_id, id
        );
        let now = now_ms();
        let dir = self
            .config
            .base_dir
            .join(sanitize_path_component(&req.stream_id))
            .join(date_dir_yyyymmdd(now));
        debug!(
            "[Snapshot] ensure output dir stream='{}' id='{}' dir='{}'",
            req.stream_id,
            id,
            dir.display()
        );
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("create {}", dir.display()))?;
        let image_path = dir.join(format!("{id}.jpg"));

        let selection = self.wait_latest_idr(&req.stream_id).await?;
        self.wait_for_pull_idr_video_window(&req.stream_id, &selection)
            .await;
        let frame = selection.frame;
        debug!(
            "[Snapshot] selected frame stream='{}' id='{}' seq={} codec={:?} ts={} keyframe={} bytes={} clock_rate={:?} wait_elapsed_ms={}",
            req.stream_id,
            id,
            selection.seq,
            frame.codec,
            frame.timestamp,
            frame.is_keyframe,
            frame.data.len(),
            frame.clock_rate,
            started.elapsed().as_millis()
        );
        if !matches!(frame.codec, CodecType::H264 | CodecType::H265) {
            return Err(anyhow!("snapshot only supports H264/H265 video stream"));
        }
        let input_format = ffmpeg_raw_video_format(frame.codec)?;
        let input_extension = raw_video_extension(frame.codec)?;
        let original_frame_bytes = frame.data.len();
        let h264_intra_fallback = selection.h264_intra_fallback;
        let prepared = prepare_video_snapshot_frame(
            &self.stream_manager,
            &req.stream_id,
            frame,
            selection.seq,
        );
        debug!(
            "[Snapshot] prepared frame stream='{}' id='{}' codec={:?} ts={} input_format='{}' raw_ext='{}' original_bytes={} prepared_bytes={}",
            req.stream_id,
            id,
            prepared.codec,
            prepared.timestamp,
            input_format,
            input_extension,
            original_frame_bytes,
            prepared.data.len()
        );
        let raw_path = dir.join(format!("{id}.{input_extension}"));
        debug!(
            "[Snapshot] write raw frame stream='{}' id='{}' path='{}' bytes={}",
            req.stream_id,
            id,
            raw_path.display(),
            prepared.data.len()
        );
        tokio::fs::write(&raw_path, &prepared.data)
            .await
            .with_context(|| format!("write {}", raw_path.display()))?;

        debug!(
            "[Snapshot] waiting ffmpeg slot stream='{}' id='{}' available_permits={}",
            req.stream_id,
            id,
            self.ffmpeg_slots.available_permits()
        );
        let _permit = self
            .ffmpeg_slots
            .acquire()
            .await
            .map_err(|_| anyhow!("snapshot ffmpeg pool closed"))?;
        debug!(
            "[Snapshot] acquired ffmpeg slot stream='{}' id='{}' available_permits={}",
            req.stream_id,
            id,
            self.ffmpeg_slots.available_permits()
        );

        let ffmpeg_path = self.config.ffmpeg_path.clone();
        let ffmpeg_timeout = self.config.ffmpeg_timeout;
        let raw_for_ffmpeg = raw_path.clone();
        let image_for_ffmpeg = image_path.clone();
        let input_format_owned = input_format.to_string();
        debug!(
            "[Snapshot] spawn ffmpeg worker stream='{}' id='{}' ffmpeg='{}' input_format='{}' raw='{}' image='{}' timeout_ms={}",
            req.stream_id,
            id,
            ffmpeg_path,
            input_format,
            raw_path.display(),
            image_path.display(),
            ffmpeg_timeout.as_millis()
        );
        tokio::task::spawn_blocking(move || {
            run_ffmpeg_snapshot_blocking(
                ffmpeg_path,
                input_format_owned,
                raw_for_ffmpeg,
                image_for_ffmpeg,
                ffmpeg_timeout,
                h264_intra_fallback,
            )
        })
        .await
        .context("ffmpeg worker join failed")??;
        debug!(
            "[Snapshot] ffmpeg worker finished stream='{}' id='{}' elapsed_ms={}",
            req.stream_id,
            id,
            started.elapsed().as_millis()
        );

        let _ = tokio::fs::remove_file(&raw_path).await;
        debug!(
            "[Snapshot] removed raw frame stream='{}' id='{}' raw='{}'",
            req.stream_id,
            id,
            raw_path.display()
        );

        let bytes = tokio::fs::metadata(&image_path)
            .await
            .map(|m| m.len())
            .unwrap_or_default();
        debug!(
            "[Snapshot] output metadata stream='{}' id='{}' image='{}' bytes={}",
            req.stream_id,
            id,
            image_path.display(),
            bytes
        );
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

    async fn wait_latest_idr(&self, stream_id: &str) -> Result<SnapshotSelection> {
        if let Some(selection) = self.find_snapshot_frame(stream_id) {
            debug!(
                "[Snapshot] using cached IDR stream='{}' seq={} codec={:?} ts={} bytes={}",
                stream_id,
                selection.seq,
                selection.frame.codec,
                selection.frame.timestamp,
                selection.frame.data.len()
            );
            return Ok(selection);
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

        let wait_budget = self.keyframe_wait_for(stream_id);
        debug!(
            "[Snapshot] wait fresh IDR stream='{}' wait_budget_ms={} latest_seq={} oldest_seq={:?} empty={}",
            stream_id,
            wait_budget.as_millis(),
            hub.latest_seq(),
            hub.oldest_seq(),
            hub.is_empty()
        );
        let wait_started = Instant::now();
        let deadline = wait_started + wait_budget;
        let mut polls = 0u64;
        while Instant::now() < deadline {
            if let Some(selection) = self.find_snapshot_frame(stream_id) {
                debug!(
                    "[Snapshot] fresh IDR ready stream='{}' seq={} codec={:?} ts={} bytes={} waited_ms={} polls={}",
                    stream_id,
                    selection.seq,
                    selection.frame.codec,
                    selection.frame.timestamp,
                    selection.frame.data.len(),
                    wait_started.elapsed().as_millis(),
                    polls
                );
                return Ok(selection);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let wait_for = remaining.min(Duration::from_millis(200));
            let _ = timeout(wait_for, notify.changed()).await;
            polls = polls.saturating_add(1);
            if polls == 1 || polls % 5 == 0 {
                debug!(
                    "[Snapshot] still waiting IDR stream='{}' waited_ms={} remaining_ms={} latest_seq={} polls={}",
                    stream_id,
                    wait_started.elapsed().as_millis(),
                    remaining.as_millis(),
                    hub.latest_seq(),
                    polls
                );
            }
        }

        if let Some(selection) = self.find_snapshot_frame(stream_id) {
            debug!(
                "[Snapshot] IDR found at deadline stream='{}' seq={} codec={:?} ts={} bytes={} waited_ms={} polls={}",
                stream_id,
                selection.seq,
                selection.frame.codec,
                selection.frame.timestamp,
                selection.frame.data.len(),
                wait_started.elapsed().as_millis(),
                polls
            );
            return Ok(selection);
        }
        debug!(
            "[Snapshot] no IDR after wait stream='{}' waited_ms={} latest_seq={} oldest_seq={:?} empty={} polls={}",
            stream_id,
            wait_started.elapsed().as_millis(),
            hub.latest_seq(),
            hub.oldest_seq(),
            hub.is_empty(),
            polls
        );
        Err(anyhow!("no playable H264/H265 keyframe available"))
    }

    async fn wait_for_pull_idr_video_window(&self, stream_id: &str, selection: &SnapshotSelection) {
        if selection.h264_intra_fallback || !selection.frame.is_keyframe {
            return;
        }
        let Some(hub) = self.stream_manager.get_hub(stream_id) else {
            return;
        };
        if hub.stream().source != StreamSourceMode::Pull {
            return;
        }

        let initial = count_snapshot_video_frames(&hub, selection.seq, &selection.frame);
        if initial >= SNAPSHOT_PULL_IDR_MIN_VIDEO_FRAMES {
            debug!(
                "[Snapshot] pull IDR video window already ready stream='{}' seq={} frames={} min_frames={} latest_seq={}",
                stream_id,
                selection.seq,
                initial,
                SNAPSHOT_PULL_IDR_MIN_VIDEO_FRAMES,
                hub.latest_seq()
            );
            return;
        }

        let mut notify = hub.subscribe_notify();
        let _baseline = *notify.borrow();
        let wait_budget = Duration::from_millis(SNAPSHOT_PULL_IDR_COLLECT_WAIT_MS);
        let started = Instant::now();
        let deadline = started + wait_budget;
        let mut frames = initial;
        let mut polls = 0u64;
        debug!(
            "[Snapshot] waiting pull IDR video window stream='{}' seq={} initial_frames={} min_frames={} wait_ms={} latest_seq={}",
            stream_id,
            selection.seq,
            initial,
            SNAPSHOT_PULL_IDR_MIN_VIDEO_FRAMES,
            wait_budget.as_millis(),
            hub.latest_seq()
        );

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let wait_for = remaining.min(Duration::from_millis(100));
            let _ = timeout(wait_for, notify.changed()).await;
            polls = polls.saturating_add(1);
            frames = count_snapshot_video_frames(&hub, selection.seq, &selection.frame);
            if frames >= SNAPSHOT_PULL_IDR_MIN_VIDEO_FRAMES {
                debug!(
                    "[Snapshot] pull IDR video window ready stream='{}' seq={} frames={} waited_ms={} polls={} latest_seq={}",
                    stream_id,
                    selection.seq,
                    frames,
                    started.elapsed().as_millis(),
                    polls,
                    hub.latest_seq()
                );
                return;
            }
        }

        debug!(
            "[Snapshot] pull IDR video window wait ended stream='{}' seq={} frames={} min_frames={} waited_ms={} polls={} latest_seq={}",
            stream_id,
            selection.seq,
            frames,
            SNAPSHOT_PULL_IDR_MIN_VIDEO_FRAMES,
            started.elapsed().as_millis(),
            polls,
            hub.latest_seq()
        );
    }

    fn find_snapshot_frame(&self, stream_id: &str) -> Option<SnapshotSelection> {
        let hub = self.stream_manager.get_hub(stream_id)?;
        if let Some(seq) = hub.latest_idr_seq() {
            if let Some(frame) = hub.get(seq) {
                if Self::is_snapshot_candidate(&frame) && is_idr_frame(&frame) {
                    debug!(
                        "[Snapshot] found latest playable IDR stream='{}' seq={} codec={:?} ts={} bytes={}",
                        stream_id,
                        seq,
                        frame.codec,
                        frame.timestamp,
                        frame.data.len()
                    );
                    return Some(SnapshotSelection {
                        frame,
                        seq,
                        h264_intra_fallback: false,
                    });
                }
            }
        }

        if hub.is_empty() {
            return None;
        }
        let latest = hub.latest_seq();
        let oldest = hub.oldest_seq().unwrap_or(0);
        let from = latest.saturating_sub(120).max(oldest);
        for seq in (from..=latest).rev() {
            let Some(stored) = hub.get_stored(seq) else {
                continue;
            };
            if !stored.is_keyframe || stored.data.is_empty() {
                continue;
            }
            let frame = stored.to_media_frame();
            if Self::is_snapshot_candidate(&frame) && is_idr_frame(&frame) {
                debug!(
                    "[Snapshot] found fallback IDR stream='{}' seq={} from_seq={} to_seq={} codec={:?} ts={} bytes={}",
                    stream_id,
                    seq,
                    from,
                    latest,
                    frame.codec,
                    frame.timestamp,
                    frame.data.len()
                );
                return Some(SnapshotSelection {
                    frame,
                    seq,
                    h264_intra_fallback: false,
                });
            }
        }

        if hub.stream().source == StreamSourceMode::Pull {
            let stream = hub.stream();
            let has_h264_config = stream.sps.is_some() && stream.pps.is_some();
            if !has_h264_config {
                debug!(
                    "[Snapshot] skip H264 intra fallback without SPS/PPS stream='{}' latest_seq={} oldest_seq={:?}",
                    stream_id,
                    latest,
                    hub.oldest_seq()
                );
                return None;
            }
            let from = latest.saturating_sub(SNAPSHOT_RING_LOOKBACK).max(oldest);
            for seq in (from..=latest).rev() {
                let Some(stored) = hub.get_stored(seq) else {
                    continue;
                };
                if stored.data.is_empty() {
                    continue;
                }
                let frame = stored.to_media_frame();
                if Self::is_snapshot_candidate(&frame) && is_snapshot_h264_intra_candidate(&frame) {
                    debug!(
                        "[Snapshot] found H264 intra fallback stream='{}' seq={} from_seq={} to_seq={} ts={} bytes={}",
                        stream_id,
                        seq,
                        from,
                        latest,
                        frame.timestamp,
                        frame.data.len()
                    );
                    return Some(SnapshotSelection {
                        frame,
                        seq,
                        h264_intra_fallback: true,
                    });
                }
            }
        }
        None
    }

    fn is_snapshot_candidate(frame: &MediaFrame) -> bool {
        is_playable_video_frame(frame) && !frame.data.is_empty()
    }

    fn keyframe_wait_for(&self, stream_id: &str) -> Duration {
        let base = self.config.wait_keyframe;
        let is_pull = self
            .stream_manager
            .get_hub(stream_id)
            .map(|hub| hub.stream().source == StreamSourceMode::Pull)
            .unwrap_or(false);
        if is_pull {
            let job_budget = self.capture_job_timeout_for(stream_id);
            let ffmpeg_budget = self.config.ffmpeg_timeout + PULL_KEYFRAME_WAIT_BUFFER;
            base
                .max(Duration::from_millis(PULL_WAIT_KEYFRAME_MS))
                .min(job_budget.saturating_sub(ffmpeg_budget))
        } else {
            base
        }
    }

    fn capture_job_timeout_for(&self, stream_id: &str) -> Duration {
        let is_pull = self
            .stream_manager
            .get_hub(stream_id)
            .map(|hub| hub.stream().source == StreamSourceMode::Pull)
            .unwrap_or(false);
        if is_pull {
            Duration::from_secs(PULL_CAPTURE_JOB_TIMEOUT_SECS)
        } else {
            Duration::from_secs(DEFAULT_CAPTURE_JOB_TIMEOUT_SECS)
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
            debug!(
                "[Snapshot] status completed stream='{}' id='{}' source_ts={} path='{}' bytes={}",
                entry.stream_id,
                id,
                source_timestamp,
                image_path.display(),
                bytes
            );
        }
    }

    fn mark_failed(&self, id: &str, error: String) {
        let mut index = self.index.write();
        if let Some(entry) = index.iter_mut().find(|entry| entry.id == id) {
            entry.completed_at_ms = Some(now_ms());
            entry.status = "error".to_string();
            entry.error = Some(error);
            debug!(
                "[Snapshot] status error stream='{}' id='{}' error='{}'",
                entry.stream_id,
                id,
                entry.error.as_deref().unwrap_or("")
            );
        }
    }
}

fn run_ffmpeg_snapshot_blocking(
    ffmpeg_path: String,
    input_format: String,
    raw_path: PathBuf,
    image_path: PathBuf,
    timeout_budget: Duration,
    h264_intra_fallback: bool,
) -> Result<()> {
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::Instant as StdInstant;

    debug!(
        "[Snapshot] ffmpeg command start ffmpeg='{}' input_format='{}' raw='{}' image='{}' timeout_ms={}",
        ffmpeg_path,
        input_format,
        raw_path.display(),
        image_path.display(),
        timeout_budget.as_millis()
    );
    let mut command = Command::new(&ffmpeg_path);
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y");
    if input_format == "h264" {
        command
            .arg("-err_detect")
            .arg("ignore_err")
            .arg("-max_error_rate")
            .arg("2")
            .arg("-flags2")
            .arg("+showall");
    }
    let mut child = command
        .arg("-f")
        .arg(input_format)
        .arg("-i")
        .arg(&raw_path)
        .args(if h264_intra_fallback {
            vec![
                "-vf".to_string(),
                format!("select=gte(n\\,{SNAPSHOT_H264_INTRA_SKIP_DECODED_FRAMES})"),
            ]
        } else {
            Vec::new()
        })
        .arg("-frames:v")
        .arg("1")
        .arg("-q:v")
        .arg("2")
        .arg("-strict")
        .arg("-2")
        .arg(&image_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {ffmpeg_path}"))?;

    let started = StdInstant::now();
    debug!(
        "[Snapshot] ffmpeg process spawned pid={:?} raw='{}' image='{}'",
        child.id(),
        raw_path.display(),
        image_path.display()
    );
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let stderr = read_child_stderr(&mut child);
                debug!(
                    "[Snapshot] ffmpeg process exited successfully status={} elapsed_ms={} image='{}' stderr='{}'",
                    status,
                    started.elapsed().as_millis(),
                    image_path.display(),
                    stderr
                );
                return Ok(());
            }
            Ok(Some(status)) => {
                let stderr = read_child_stderr(&mut child);
                debug!(
                    "[Snapshot] ffmpeg process failed status={} elapsed_ms={} raw='{}' image='{}' stderr='{}'",
                    status,
                    started.elapsed().as_millis(),
                    raw_path.display(),
                    image_path.display(),
                    stderr
                );
                return Err(anyhow!("ffmpeg snapshot failed: exit {status}: {stderr}"));
            }
            Ok(None) => {
                if started.elapsed() >= timeout_budget {
                    let _ = child.kill();
                    let _ = child.wait();
                    let stderr = read_child_stderr(&mut child);
                    debug!(
                        "[Snapshot] ffmpeg process timeout elapsed_ms={} timeout_ms={} raw='{}' image='{}' stderr='{}'",
                        started.elapsed().as_millis(),
                        timeout_budget.as_millis(),
                        raw_path.display(),
                        image_path.display(),
                        stderr
                    );
                    return Err(anyhow!(
                        "ffmpeg snapshot timed out after {}s: {}",
                        timeout_budget.as_secs(),
                        stderr
                    ));
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                debug!(
                    "[Snapshot] ffmpeg wait error raw='{}' image='{}' error='{}'",
                    raw_path.display(),
                    image_path.display(),
                    e
                );
                return Err(anyhow!("ffmpeg wait error: {e}"));
            }
        }
    }
}

fn read_child_stderr(child: &mut std::process::Child) -> String {
    use std::io::Read;

    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    stderr.trim().replace('\n', " | ")
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
    idr_seq: u64,
) -> MediaFrame {
    let au = collect_snapshot_video_annex_b(manager, stream_id, &frame, idr_seq);
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

fn collect_snapshot_video_annex_b(
    manager: &StreamManager,
    stream_id: &str,
    selected: &MediaFrame,
    idr_seq: u64,
) -> Vec<u8> {
    let Some(hub) = manager.get_hub(stream_id) else {
        return ensure_annex_b(&selected.data);
    };

    let latest = hub.latest_seq();
    let oldest = hub.oldest_seq().unwrap_or(idr_seq);
    let use_preroll = !selected.is_keyframe
        && selected.codec == CodecType::H264
        && is_snapshot_h264_intra_candidate(selected);
    let start_seq = if use_preroll {
        idr_seq
            .saturating_sub(SNAPSHOT_H264_INTRA_PREROLL_FRAMES)
            .max(oldest)
    } else {
        idr_seq
    };
    let mut out = Vec::new();
    let mut frames = 0usize;
    let mut bytes = 0usize;
    let mut stopped_at_next_gop = false;

    for seq in start_seq..=latest {
        let Some(frame) = hub.get(seq) else {
            continue;
        };
        if frame.track_id != selected.track_id || frame.codec != selected.codec {
            continue;
        }
        if !use_preroll && frames > 0 && frame.is_keyframe {
            stopped_at_next_gop = true;
            break;
        }
        let annex = ensure_annex_b(&frame.data);
        if annex.is_empty() {
            continue;
        }
        if frames > 0 && bytes.saturating_add(annex.len()) > SNAPSHOT_MAX_RAW_BYTES {
            break;
        }
        bytes = bytes.saturating_add(annex.len());
        out.extend_from_slice(&annex);
        frames += 1;
        if frames >= SNAPSHOT_MAX_VIDEO_FRAMES {
            break;
        }
    }

    if out.is_empty() {
        out = ensure_annex_b(&selected.data);
        frames = usize::from(!out.is_empty());
        bytes = out.len();
    }

    debug!(
        "[Snapshot] collected raw video stream='{}' idr_seq={} start_seq={} latest_seq={} frames={} bytes={} codec={:?} preroll={} stopped_at_next_gop={}",
        stream_id,
        idr_seq,
        start_seq,
        latest,
        frames,
        bytes,
        selected.codec,
        use_preroll,
        stopped_at_next_gop
    );
    out
}

fn count_snapshot_video_frames(hub: &Arc<StreamHub>, idr_seq: u64, selected: &MediaFrame) -> usize {
    let latest = hub.latest_seq();
    let mut frames = 0usize;
    for seq in idr_seq..=latest {
        let Some(frame) = hub.get(seq) else {
            continue;
        };
        if frame.track_id != selected.track_id || frame.codec != selected.codec {
            continue;
        }
        if frames > 0 && frame.is_keyframe {
            break;
        }
        if ensure_annex_b(&frame.data).is_empty() {
            continue;
        }
        frames += 1;
        if frames >= SNAPSHOT_PULL_IDR_MIN_VIDEO_FRAMES {
            break;
        }
    }
    frames
}

fn is_snapshot_h264_intra_candidate(frame: &MediaFrame) -> bool {
    if frame.codec != CodecType::H264 {
        return false;
    }
    let annex_b = ensure_annex_b(&frame.data);
    iter_annex_b_nal_ranges(&annex_b)
        .into_iter()
        .any(|(start, end)| h264_slice_type(&annex_b[start..end]).is_some_and(is_h264_intra_slice))
}

fn is_h264_intra_slice(slice_type: u32) -> bool {
    matches!(slice_type % 5, 2 | 4)
}

fn h264_slice_type(nal: &[u8]) -> Option<u32> {
    if nal.len() < 2 || (nal[0] & 0x1F) != 1 {
        return None;
    }
    let rbsp = h264_rbsp_from_ebsp(&nal[1..]);
    let mut reader = BitReader::new(&rbsp);
    let _first_mb_in_slice = reader.read_ue()?;
    reader.read_ue()
}

fn h264_rbsp_from_ebsp(ebsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ebsp.len());
    let mut zeros = 0usize;
    for &byte in ebsp {
        if zeros >= 2 && byte == 0x03 {
            zeros = 0;
            continue;
        }
        out.push(byte);
        if byte == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
    }
    out
}

struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn read_bit(&mut self) -> Option<u8> {
        if self.bit_pos >= self.data.len() * 8 {
            return None;
        }
        let byte = self.data[self.bit_pos / 8];
        let shift = 7 - (self.bit_pos % 8);
        self.bit_pos += 1;
        Some((byte >> shift) & 1)
    }

    fn read_ue(&mut self) -> Option<u32> {
        let mut leading_zero_bits = 0u32;
        while self.read_bit()? == 0 {
            leading_zero_bits += 1;
            if leading_zero_bits > 31 {
                return None;
            }
        }
        let mut value = 1u32 << leading_zero_bits;
        for shift in (0..leading_zero_bits).rev() {
            value |= (self.read_bit()? as u32) << shift;
        }
        Some(value - 1)
    }
}

fn sanitize_path_component(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else if c.is_control() || matches!(c, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "_".to_string()
    } else {
        trimmed.to_string()
    }
}

fn snapshot_id_component(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "stream".to_string()
    } else {
        trimmed.to_string()
    }
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
        assert_eq!(sanitize_path_component("重庆卫视"), "重庆卫视");
        assert_eq!(sanitize_path_component("大湾区/卫视"), "大湾区_卫视");
    }

    #[test]
    fn snapshot_id_component_is_url_and_file_safe() {
        assert_eq!(snapshot_id_component("cam/a:b"), "cam_a_b");
        assert_eq!(snapshot_id_component("大湾区/卫视"), "stream");
        assert_eq!(snapshot_id_component("cam-01_main"), "cam-01_main");
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

        let prepared = prepare_video_snapshot_frame(&manager, "s", frame, 0);

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

        let prepared = prepare_video_snapshot_frame(&manager, "s", frame, 0);

        assert!(
            prepared
                .data
                .windows(8)
                .any(|w| w == [0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01])
        );
        assert!(
            prepared
                .data
                .windows(8)
                .any(|w| w == [0, 0, 0, 1, 0x42, 0x01, 0x01, 0x60])
        );
        assert!(
            prepared
                .data
                .windows(8)
                .any(|w| w == [0, 0, 0, 1, 0x44, 0x01, 0xc0, 0xf1])
        );
        assert!(
            prepared
                .data
                .windows(7)
                .any(|w| w == [0, 0, 0, 1, 0x26, 0x01, 0xaa])
        );
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

    #[test]
    fn snapshot_frame_includes_following_video_frames_from_idr() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Pull, StreamProtocol::HLS, None);
        manager.set_stream_sps_pps("s", vec![0x67, 0x42], vec![0x68, 0xce]);

        let idr = MediaFrame::new(
            "s".to_string(),
            0,
            1_000,
            Bytes::from_static(&[0, 0, 0, 1, 0x65, 0x88]),
            true,
            CodecType::H264,
        );
        let p_frame = MediaFrame::new(
            "s".to_string(),
            0,
            1_033,
            Bytes::from_static(&[0, 0, 0, 1, 0x41, 0x9a]),
            false,
            CodecType::H264,
        );

        manager.publish_frame(idr.clone());
        manager.publish_frame(p_frame);

        let prepared = prepare_video_snapshot_frame(&manager, "s", idr, 0);

        assert!(prepared.data.windows(5).any(|w| w == [0, 0, 0, 1, 0x65]));
        assert!(prepared.data.windows(5).any(|w| w == [0, 0, 0, 1, 0x41]));
    }

    #[tokio::test]
    async fn pull_idr_snapshot_waits_for_following_video_window() {
        let stream_manager = Arc::new(StreamManager::new());
        stream_manager.create_stream("s", StreamSourceMode::Pull, StreamProtocol::HLS, None);
        stream_manager.set_stream_sps_pps("s", vec![0x67, 0x42], vec![0x68, 0xce]);

        let idr = MediaFrame::new(
            "s".to_string(),
            0,
            1_000,
            Bytes::from_static(&[0, 0, 0, 1, 0x65, 0x88]),
            true,
            CodecType::H264,
        );
        stream_manager.publish_frame(idr.clone());

        let publisher = stream_manager.clone();
        tokio::spawn(async move {
            for i in 1..SNAPSHOT_PULL_IDR_MIN_VIDEO_FRAMES {
                tokio::time::sleep(Duration::from_millis(10)).await;
                publisher.publish_frame(MediaFrame::new(
                    "s".to_string(),
                    0,
                    1_000 + i as u64 * 33,
                    Bytes::from_static(&[0, 0, 0, 1, 0x41, 0x9a]),
                    false,
                    CodecType::H264,
                ));
            }
        });

        let snapshot_manager =
            SnapshotManager::new(stream_manager.clone(), SnapshotConfig::default());
        let selection = SnapshotSelection {
            frame: idr,
            seq: 0,
            h264_intra_fallback: false,
        };

        snapshot_manager
            .wait_for_pull_idr_video_window("s", &selection)
            .await;

        let hub = stream_manager.get_hub("s").expect("hub");
        assert_eq!(
            count_snapshot_video_frames(&hub, 0, &selection.frame),
            SNAPSHOT_PULL_IDR_MIN_VIDEO_FRAMES
        );
    }

    #[test]
    fn hls_snapshot_can_fallback_to_h264_intra_slice_without_idr() {
        let stream_manager = Arc::new(StreamManager::new());
        stream_manager.create_stream("s", StreamSourceMode::Pull, StreamProtocol::HLS, None);
        stream_manager.set_stream_sps_pps("s", vec![0x67, 0x42], vec![0x68, 0xce]);
        let p_frame = MediaFrame::new(
            "s".to_string(),
            0,
            1_000,
            Bytes::from_static(&[0, 0, 0, 1, 0x41, 0x9a]),
            false,
            CodecType::H264,
        );
        let i_slice = MediaFrame::new(
            "s".to_string(),
            0,
            1_033,
            Bytes::from_static(&[0, 0, 0, 1, 0x41, 0xb0]),
            false,
            CodecType::H264,
        );
        stream_manager.publish_frame(p_frame);
        stream_manager.publish_frame(i_slice);

        let snapshot = SnapshotManager::new(stream_manager, SnapshotConfig::default());
        let selection = snapshot
            .find_snapshot_frame("s")
            .expect("H264 I-slice should be usable as snapshot fallback");

        assert_eq!(selection.seq, 1);
        assert!(!selection.frame.is_keyframe);
        assert!(is_snapshot_h264_intra_candidate(&selection.frame));
    }

    #[test]
    fn h264_intra_fallback_collects_preroll_frames() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Pull, StreamProtocol::HLS, None);
        manager.set_stream_sps_pps("s", vec![0x67, 0x42], vec![0x68, 0xce]);
        let preroll = MediaFrame::new(
            "s".to_string(),
            0,
            1_000,
            Bytes::from_static(&[0, 0, 0, 1, 0x41, 0x9a]),
            false,
            CodecType::H264,
        );
        let i_slice = MediaFrame::new(
            "s".to_string(),
            0,
            1_033,
            Bytes::from_static(&[0, 0, 0, 1, 0x41, 0xb0]),
            false,
            CodecType::H264,
        );
        manager.publish_frame(preroll);
        manager.publish_frame(i_slice.clone());

        let collected = collect_snapshot_video_annex_b(&manager, "s", &i_slice, 1);

        assert!(collected.windows(6).any(|w| w == [0, 0, 0, 1, 0x41, 0x9a]));
        assert!(collected.windows(6).any(|w| w == [0, 0, 0, 1, 0x41, 0xb0]));
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

    #[tokio::test]
    async fn submit_snapshot_uses_safe_id_for_stream_name_with_slash() {
        let stream_manager = Arc::new(StreamManager::new());
        stream_manager.create_stream(
            "大湾区/卫视",
            StreamSourceMode::Pull,
            StreamProtocol::HLS,
            None,
        );
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
                stream_id: "大湾区/卫视".to_string(),
            })
            .expect("submit snapshot");

        assert!(entry.id.starts_with("snap_stream_"));
        assert!(!entry.id.contains('/'));
        assert!(manager.get(&entry.id).is_some());
    }
}
