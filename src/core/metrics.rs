//! Server-wide operational metrics for HTTP monitoring APIs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use serde::Serialize;

use super::StreamManager;

static GLOBAL: parking_lot::Mutex<Option<Arc<MetricsRegistry>>> = parking_lot::const_mutex(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackProtocol {
    Rtmp,
    Rtsp,
    HttpFlv,
    Hls,
    Webrtc,
}

impl PlaybackProtocol {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rtmp => "rtmp",
            Self::Rtsp => "rtsp",
            Self::HttpFlv => "http_flv",
            Self::Hls => "hls",
            Self::Webrtc => "webrtc",
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct FirstFrameStats {
    pub count: u64,
    pub last_ms: Option<u64>,
    pub avg_ms: f64,
    pub max_ms: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PlayerCounts {
    pub rtmp: u64,
    pub rtsp: u64,
    pub http_flv: u64,
    pub hls: u64,
    pub webrtc: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RingStats {
    pub frame_count: usize,
    pub bytes_used: usize,
    pub oldest_seq: Option<u64>,
    pub latest_seq: u64,
    pub latest_idr_seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct HlsMetricsSnapshot {
    pub generating: bool,
    pub segment_count: usize,
    pub media_sequence: u64,
    pub target_duration: f64,
    pub max_segments: usize,
    pub last_segment_duration: Option<f64>,
    pub last_segment_seq: Option<u64>,
    pub last_segment_bytes: Option<usize>,
    pub committed_segments: u64,
    pub last_access_age_ms: Option<u64>,
    pub playlist_requests: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamMetricsSnapshot {
    pub stream_id: String,
    pub stream_status: String,
    pub playback_status: String,
    pub keyframe_requests: u64,
    pub keyframe_requests_delivered: u64,
    pub last_keyframe_request_age_ms: Option<u64>,
    pub ring_gaps: u64,
    pub rtp_sequence_gaps: u64,
    pub players: PlayerCounts,
    pub first_frame_ms: HashMap<String, FirstFrameStats>,
    pub ring: RingStats,
    pub hls: HlsMetricsSnapshot,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub updated_at_ms: u64,
    pub stream_count: usize,
    pub streams: Vec<StreamMetricsSnapshot>,
}

struct FirstFrameAccumulator {
    count: u64,
    sum_ms: u64,
    max_ms: u64,
    last_ms: Option<u64>,
}

impl Default for FirstFrameAccumulator {
    fn default() -> Self {
        Self {
            count: 0,
            sum_ms: 0,
            max_ms: 0,
            last_ms: None,
        }
    }
}

impl FirstFrameAccumulator {
    fn record(&mut self, ms: u64) {
        self.count += 1;
        self.sum_ms += ms;
        self.max_ms = self.max_ms.max(ms);
        self.last_ms = Some(ms);
    }

    fn snapshot(&self) -> FirstFrameStats {
        FirstFrameStats {
            count: self.count,
            last_ms: self.last_ms,
            avg_ms: if self.count == 0 {
                0.0
            } else {
                self.sum_ms as f64 / self.count as f64
            },
            max_ms: self.max_ms,
        }
    }
}

struct StreamMetrics {
    keyframe_requests: AtomicU64,
    keyframe_requests_delivered: AtomicU64,
    ring_gaps: AtomicU64,
    rtp_sequence_gaps: AtomicU64,
    last_keyframe_request: RwLock<Option<Instant>>,
    players: [AtomicU64; 5],
    first_frame: RwLock<HashMap<PlaybackProtocol, FirstFrameAccumulator>>,
    hls: RwLock<HlsMetricsSnapshot>,
    last_hls_access: RwLock<Option<Instant>>,
}

impl Default for StreamMetrics {
    fn default() -> Self {
        Self {
            keyframe_requests: AtomicU64::new(0),
            keyframe_requests_delivered: AtomicU64::new(0),
            ring_gaps: AtomicU64::new(0),
            rtp_sequence_gaps: AtomicU64::new(0),
            last_keyframe_request: RwLock::new(None),
            players: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            first_frame: RwLock::new(HashMap::new()),
            hls: RwLock::new(HlsMetricsSnapshot::default()),
            last_hls_access: RwLock::new(None),
        }
    }
}

pub struct MetricsRegistry {
    streams: RwLock<HashMap<String, Arc<StreamMetrics>>>,
}

impl MetricsRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            streams: RwLock::new(HashMap::new()),
        })
    }

    pub fn install_global(registry: Arc<Self>) {
        *GLOBAL.lock() = Some(registry);
    }

    pub fn global() -> Option<Arc<Self>> {
        GLOBAL.lock().clone()
    }

    fn stream(&self, stream_id: &str) -> Arc<StreamMetrics> {
        let mut streams = self.streams.write();
        streams
            .entry(stream_id.to_string())
            .or_insert_with(|| Arc::new(StreamMetrics::default()))
            .clone()
    }

    pub fn record_keyframe_request(stream_id: &str, delivered: bool) {
        let Some(registry) = Self::global() else {
            return;
        };
        let metrics = registry.stream(stream_id);
        metrics
            .keyframe_requests
            .fetch_add(1, Ordering::Relaxed);
        if delivered {
            metrics
                .keyframe_requests_delivered
                .fetch_add(1, Ordering::Relaxed);
        }
        *metrics.last_keyframe_request.write() = Some(Instant::now());
    }

    pub fn record_ring_gap(stream_id: &str) {
        let Some(registry) = Self::global() else {
            return;
        };
        registry
            .stream(stream_id)
            .ring_gaps
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_rtp_sequence_gap(stream_id: &str) {
        let Some(registry) = Self::global() else {
            return;
        };
        registry
            .stream(stream_id)
            .rtp_sequence_gaps
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_player(stream_id: &str, protocol: PlaybackProtocol) {
        let Some(registry) = Self::global() else {
            return;
        };
        registry
            .stream(stream_id)
            .players[protocol_index(protocol)]
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_player(stream_id: &str, protocol: PlaybackProtocol) {
        let Some(registry) = Self::global() else {
            return;
        };
        let metrics = registry.stream(stream_id);
        let idx = protocol_index(protocol);
        loop {
            let current = metrics.players[idx].load(Ordering::Relaxed);
            if current == 0 {
                break;
            }
            if metrics.players[idx]
                .compare_exchange(current, current - 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
    }

    pub fn record_first_frame_ms(stream_id: &str, protocol: PlaybackProtocol, ms: u64) {
        let Some(registry) = Self::global() else {
            return;
        };
        registry
            .stream(stream_id)
            .first_frame
            .write()
            .entry(protocol)
            .or_default()
            .record(ms);
    }

    pub fn record_hls_access(stream_id: &str) {
        let Some(registry) = Self::global() else {
            return;
        };
        let metrics = registry.stream(stream_id);
        *metrics.last_hls_access.write() = Some(Instant::now());
        let mut hls = metrics.hls.write();
        hls.playlist_requests = hls.playlist_requests.saturating_add(1);
    }

    pub fn update_hls(stream_id: &str, update: HlsMetricsSnapshot) {
        let Some(registry) = Self::global() else {
            return;
        };
        *registry.stream(stream_id).hls.write() = update;
    }

    pub fn merge_hls(stream_id: &str, mut update: HlsMetricsSnapshot) {
        let Some(registry) = Self::global() else {
            return;
        };
        let metrics = registry.stream(stream_id);
        let mut current = metrics.hls.write();
        if update.playlist_requests == 0 {
            update.playlist_requests = current.playlist_requests;
        }
        if update.committed_segments == 0 {
            update.committed_segments = current.committed_segments;
        }
        if update.last_segment_duration.is_none() {
            update.last_segment_duration = current.last_segment_duration;
        }
        if update.last_segment_seq.is_none() {
            update.last_segment_seq = current.last_segment_seq;
        }
        if update.last_segment_bytes.is_none() {
            update.last_segment_bytes = current.last_segment_bytes;
        }
        *current = update;
    }

    pub fn record_hls_segment_committed(
        stream_id: &str,
        seq: u64,
        duration: f64,
        bytes: usize,
    ) {
        let Some(registry) = Self::global() else {
            return;
        };
        let metrics = registry.stream(stream_id);
        let mut hls = metrics.hls.write();
        hls.committed_segments = hls.committed_segments.saturating_add(1);
        hls.last_segment_seq = Some(seq);
        hls.last_segment_duration = Some(duration);
        hls.last_segment_bytes = Some(bytes);
    }

    pub fn remove_stream(stream_id: &str) {
        let Some(registry) = Self::global() else {
            return;
        };
        registry.streams.write().remove(stream_id);
    }

    pub fn snapshot(&self, stream_manager: &StreamManager) -> MetricsSnapshot {
        let now_ms = now_ms();
        let stream_ids = stream_manager.list_streams();
        let mut streams = Vec::with_capacity(stream_ids.len());

        for stream_id in stream_ids {
            if let Some(entry) = self.build_stream_snapshot(stream_manager, &stream_id, now_ms) {
                streams.push(entry);
            }
        }

        MetricsSnapshot {
            updated_at_ms: now_ms,
            stream_count: streams.len(),
            streams,
        }
    }

    pub fn stream_snapshot(
        &self,
        stream_manager: &StreamManager,
        stream_id: &str,
    ) -> Option<StreamMetricsSnapshot> {
        self.build_stream_snapshot(stream_manager, stream_id, now_ms())
    }

    fn build_stream_snapshot(
        &self,
        stream_manager: &StreamManager,
        stream_id: &str,
        now_ms: u64,
    ) -> Option<StreamMetricsSnapshot> {
        let stream = stream_manager.get_stream(&stream_id.to_string())?;
        let hub = stream_manager.get_hub(stream_id)?;

        let stored = self.streams.read().get(stream_id).cloned();
        let (
            keyframe_requests,
            keyframe_requests_delivered,
            ring_gaps,
            rtp_sequence_gaps,
            last_keyframe_request_age_ms,
            players,
            first_frame_ms,
            mut hls,
        ) = if let Some(metrics) = stored {
            let players_raw = [
                metrics.players[0].load(Ordering::Relaxed),
                metrics.players[1].load(Ordering::Relaxed),
                metrics.players[2].load(Ordering::Relaxed),
                metrics.players[3].load(Ordering::Relaxed),
                metrics.players[4].load(Ordering::Relaxed),
            ];
            let webrtc_players = players_raw[4];
            let mut players = PlayerCounts {
                rtmp: players_raw[0],
                rtsp: players_raw[1],
                http_flv: players_raw[2],
                hls: players_raw[3],
                webrtc: webrtc_players,
                total: 0,
            };
            players.total = players.rtmp
                + players.rtsp
                + players.http_flv
                + players.hls
                + players.webrtc;

            let first_frame_ms = metrics
                .first_frame
                .read()
                .iter()
                .map(|(protocol, acc)| (protocol.as_str().to_string(), acc.snapshot()))
                .collect();

            let mut hls = metrics.hls.read().clone();
            if let Some(access) = metrics.last_hls_access.read().as_ref() {
                hls.last_access_age_ms = Some(access.elapsed().as_millis() as u64);
            }
            (
                metrics.keyframe_requests.load(Ordering::Relaxed),
                metrics
                    .keyframe_requests_delivered
                    .load(Ordering::Relaxed),
                metrics.ring_gaps.load(Ordering::Relaxed),
                metrics.rtp_sequence_gaps.load(Ordering::Relaxed),
                metrics
                    .last_keyframe_request
                    .read()
                    .as_ref()
                    .map(|t| t.elapsed().as_millis() as u64),
                players,
                first_frame_ms,
                hls,
            )
        } else {
            (
                0,
                0,
                0,
                0,
                None,
                PlayerCounts::default(),
                HashMap::new(),
                HlsMetricsSnapshot::default(),
            )
        };

        let ring = RingStats {
            frame_count: hub.ring_frame_count(),
            bytes_used: hub.ring_bytes_used(),
            oldest_seq: hub.oldest_seq(),
            latest_seq: hub.latest_seq(),
            latest_idr_seq: hub.latest_idr_seq(),
        };

        Some(StreamMetricsSnapshot {
            stream_id: stream_id.to_string(),
            stream_status: stream.status.as_str().to_string(),
            playback_status: stream.playback_status.as_str().to_string(),
            keyframe_requests,
            keyframe_requests_delivered,
            last_keyframe_request_age_ms,
            ring_gaps,
            rtp_sequence_gaps,
            players,
            first_frame_ms,
            ring,
            hls,
            updated_at_ms: now_ms,
        })
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self {
            streams: RwLock::new(HashMap::new()),
        }
    }
}

fn protocol_index(protocol: PlaybackProtocol) -> usize {
    match protocol {
        PlaybackProtocol::Rtmp => 0,
        PlaybackProtocol::Rtsp => 1,
        PlaybackProtocol::HttpFlv => 2,
        PlaybackProtocol::Hls => 3,
        PlaybackProtocol::Webrtc => 4,
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{StreamProtocol, StreamSourceMode};

    #[test]
    fn counters_increment_and_snapshot() {
        let registry = MetricsRegistry::new();
        MetricsRegistry::install_global(registry.clone());
        let manager = StreamManager::new();
        manager.create_stream("s1", StreamSourceMode::Push, StreamProtocol::RTMP, None);

        MetricsRegistry::record_keyframe_request("s1", true);
        MetricsRegistry::record_ring_gap("s1");
        MetricsRegistry::record_rtp_sequence_gap("s1");
        MetricsRegistry::inc_player("s1", PlaybackProtocol::Rtmp);
        MetricsRegistry::record_first_frame_ms("s1", PlaybackProtocol::Rtmp, 42);

        let snap = registry.stream_snapshot(&manager, "s1").expect("snapshot");
        assert_eq!(snap.keyframe_requests, 1);
        assert_eq!(snap.keyframe_requests_delivered, 1);
        assert_eq!(snap.ring_gaps, 1);
        assert_eq!(snap.rtp_sequence_gaps, 1);
        assert_eq!(snap.players.rtmp, 1);
        assert_eq!(snap.first_frame_ms["rtmp"].last_ms, Some(42));
    }

    #[test]
    fn merge_hls_preserves_last_segment_fields() {
        let registry = MetricsRegistry::new();
        MetricsRegistry::install_global(registry.clone());

        MetricsRegistry::record_hls_segment_committed("s1", 42, 1.0, 8192);
        MetricsRegistry::merge_hls(
            "s1",
            HlsMetricsSnapshot {
                generating: true,
                segment_count: 3,
                media_sequence: 10,
                ..Default::default()
            },
        );

        let hls = registry.streams.read().get("s1").unwrap().hls.read().clone();
        assert_eq!(hls.last_segment_seq, Some(42));
        assert_eq!(hls.last_segment_duration, Some(1.0));
        assert_eq!(hls.last_segment_bytes, Some(8192));
        assert_eq!(hls.committed_segments, 1);
        assert_eq!(hls.segment_count, 3);
    }
}
