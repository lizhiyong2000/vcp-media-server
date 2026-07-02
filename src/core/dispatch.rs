//! FrameDispatcher: per-subscriber read cursor + policy-driven frame delivery.

use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::info;

use super::frame_ring::{is_playable_video, is_video_keyframe, SnapMode};
use super::stream_hub::StreamHub;
use super::{CodecType, MediaFrame, StreamManager};
use crate::webrtc::{h264_util::is_keyframe_annex_b, request_publisher_keyframe};

/// Max frames per batch for sequential muxers (HLS); keeps pace with live ingest.
const MAX_SEQUENTIAL_BATCH: u64 = 96;
/// WebRTC play: only snap to live edge when this many frames behind (~1.5s @ 30fps).
const WEBRTC_LAG_SNAP_THRESHOLD: u64 = 45;
/// Reuse an existing IDR only when it is close to live edge; otherwise wait for a fresh one.
const PRIME_EXISTING_IDR_MAX_LAG: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchPolicy {
    /// RTMP / HTTP-FLV: live edge, coalesce video bursts, audio in order.
    LiveCoalesce,
    /// HLS / RTSP PLAY: sequential from IDR, no skip.
    SequentialFromIdr,
    /// WebRTC play: IDR start, then coalesce video like live.
    WebRtcPlay,
    /// Live playback from edge, sequential after subscribe, no coalesce.
    LiveSequential,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchError {
    Closed,
}

pub struct DispatchReader {
    hub: std::sync::Arc<StreamHub>,
    stream_id: String,
    policy: DispatchPolicy,
    cursor: u64,
    wake: watch::Receiver<u64>,
    primed: bool,
    pending_muxer_resync: bool,
    pending_live_snap: bool,
}

impl DispatchReader {
    pub fn new(hub: std::sync::Arc<StreamHub>, policy: DispatchPolicy) -> Self {
        let wake = hub.subscribe_notify();
        let stream_id = hub.stream_id.clone();
        let cursor = match policy {
            DispatchPolicy::LiveCoalesce
            | DispatchPolicy::WebRtcPlay
            | DispatchPolicy::LiveSequential => hub.snap(SnapMode::LiveEdge).saturating_add(1),
            DispatchPolicy::SequentialFromIdr => hub.snap(SnapMode::LatestIdr),
        };
        Self {
            hub,
            stream_id,
            policy,
            cursor,
            wake,
            primed: false,
            pending_muxer_resync: false,
            pending_live_snap: false,
        }
    }

    pub fn hub(&self) -> &std::sync::Arc<StreamHub> {
        &self.hub
    }

    pub fn policy(&self) -> DispatchPolicy {
        self.policy
    }

    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    pub fn snap_to_live_edge(&mut self) {
        self.cursor = self.hub.snap(SnapMode::LiveEdge).saturating_add(1);
    }

    pub fn snap_to_latest_idr(&mut self) {
        self.cursor = self.hub.snap(SnapMode::LatestIdr);
    }

    /// Wait for IDR, then position cursor at latest IDR seq.
    pub async fn prime_from_idr(
        &mut self,
        manager: &StreamManager,
        stream_id: &str,
    ) -> Option<MediaFrame> {
        let baseline_latest = self.hub.latest_seq();
        request_publisher_keyframe(stream_id);
        let deadline = Instant::now() + Duration::from_millis(800);

        while Instant::now() < deadline {
            if let Some(idr_seq) = self.hub.latest_idr_seq() {
                let fresh_after_request = idr_seq > baseline_latest;
                let close_to_live =
                    baseline_latest.saturating_sub(idr_seq) <= PRIME_EXISTING_IDR_MAX_LAG;
                if fresh_after_request || close_to_live {
                    if let Some(idr) = self.hub.get(idr_seq).filter(is_playable_idr) {
                        self.cursor = idr_seq;
                        self.primed = true;
                        info!(
                            "[Dispatch] [{}] primed IDR ts={} seq={} fresh={} live_lag={}",
                            stream_id,
                            idr.timestamp,
                            self.cursor,
                            fresh_after_request,
                            self.hub.latest_seq().saturating_sub(idr_seq)
                        );
                        return Some(idr);
                    }
                } else {
                    info!(
                        "[Dispatch] [{}] latest IDR seq={} is {} frames behind live; waiting for fresh IDR",
                        stream_id,
                        idr_seq,
                        baseline_latest.saturating_sub(idr_seq)
                    );
                }
            }
            if self.wait_new_frames(deadline).await.is_err() {
                break;
            }
        }

        if let Some(idr_seq) = self.hub.latest_idr_seq() {
            if let Some(idr) = self.hub.get(idr_seq).filter(is_playable_idr) {
                self.cursor = idr_seq;
                self.primed = true;
                info!(
                    "[Dispatch] [{}] primed fallback IDR ts={} seq={} live_lag={}",
                    stream_id,
                    idr.timestamp,
                    self.cursor,
                    self.hub.latest_seq().saturating_sub(idr_seq)
                );
                return Some(idr);
            }
        }

        // Merge SPS/PPS from stream metadata when ring IDR lacks config
        if let Some((data, ts)) = manager.get_last_keyframe(stream_id) {
            self.cursor = self.hub.snap(SnapMode::LatestIdr);
            self.primed = true;
            return Some(MediaFrame::new(
                stream_id.to_string(),
                0,
                ts,
                bytes::Bytes::from(data),
                true,
                CodecType::H264,
            ));
        }

        None
    }

    /// After priming, align cursor for the policy (HLS: latest IDR, live play: edge).
    pub fn finish_prime(&mut self) {
        match self.policy {
            DispatchPolicy::SequentialFromIdr => self.snap_to_latest_idr(),
            _ => self.snap_to_live_edge(),
        }
    }

    /// True when the last `recv_batch` snapped due to lag (sequential muxers should reset).
    pub fn take_muxer_resync(&mut self) -> bool {
        std::mem::take(&mut self.pending_muxer_resync)
    }

    /// True when a live-edge snap dropped lagged frames (HLS should reset open segment).
    pub fn take_live_snap(&mut self) -> bool {
        std::mem::take(&mut self.pending_live_snap)
    }

    /// Read next batch according to policy.
    pub async fn recv_batch(&mut self) -> Result<Vec<MediaFrame>, DispatchError> {
        self.ensure_data().await?;
        let mut latest = self.hub.latest_seq();
        let lag = latest.saturating_sub(self.cursor);

        match self.policy {
            DispatchPolicy::LiveCoalesce => {
                if lag > 0 {
                    if lag > 1 {
                        info!(
                            "[Dispatch] [{}] live lag {} frames — snap to edge",
                            self.stream_id, lag
                        );
                        request_publisher_keyframe(&self.stream_id);
                        self.pending_live_snap = true;
                    }
                    self.snap_to_live_edge();
                    latest = self.hub.latest_seq();
                }
            }
            DispatchPolicy::WebRtcPlay if lag > WEBRTC_LAG_SNAP_THRESHOLD => {
                info!(
                    "[Dispatch] [{}] WebRTC lag {} frames — snap to edge",
                    self.stream_id, lag
                );
                request_publisher_keyframe(&self.stream_id);
                self.pending_live_snap = true;
                self.snap_to_live_edge();
                latest = self.hub.latest_seq();
            }
            DispatchPolicy::WebRtcPlay => {}
            // Sequential readers preserve every frame while data remains in the ring.
            DispatchPolicy::LiveSequential | DispatchPolicy::SequentialFromIdr => {}
        }

        if self.cursor > latest {
            return Ok(Vec::new());
        }

        let batch_end = match self.policy {
            DispatchPolicy::SequentialFromIdr | DispatchPolicy::LiveSequential => latest.min(
                self.cursor
                    .saturating_add(MAX_SEQUENTIAL_BATCH.saturating_sub(1)),
            ),
            DispatchPolicy::LiveCoalesce | DispatchPolicy::WebRtcPlay => self.cursor,
        };

        let frames = self.hub.frames_from(self.cursor, batch_end);
        if frames.is_empty() && self.cursor <= latest {
            if let Some(oldest) = self.hub.oldest_seq() {
                if oldest > self.cursor {
                    info!(
                        "[Dispatch] [{}] ring gap at seq {} (oldest {}) — jump to IDR",
                        self.stream_id, self.cursor, oldest
                    );
                    request_publisher_keyframe(&self.stream_id);
                    self.pending_muxer_resync = true;
                    self.pending_live_snap = true;
                    self.cursor = self.hub.snap(SnapMode::LatestIdr).max(oldest);
                    let latest = self.hub.latest_seq();
                    if self.cursor <= latest {
                        let end = latest.min(
                            self.cursor
                                .saturating_add(MAX_SEQUENTIAL_BATCH.saturating_sub(1)),
                        );
                        let retry = self.hub.frames_from(self.cursor, end);
                        if !retry.is_empty() {
                            self.cursor = end.saturating_add(1);
                            return Ok(retry);
                        }
                    }
                }
            }
        }
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        self.cursor = batch_end.saturating_add(1);

        Ok(match self.policy {
            DispatchPolicy::LiveCoalesce | DispatchPolicy::WebRtcPlay => coalesce_flv_batch(frames),
            DispatchPolicy::LiveSequential | DispatchPolicy::SequentialFromIdr => frames,
        })
    }

    /// Single frame, coalescing any burst of video to the latest playable frame.
    pub async fn recv_coalesced(&mut self) -> Result<MediaFrame, DispatchError> {
        loop {
            let batch = self.recv_batch().await?;
            if batch.is_empty() {
                self.ensure_data().await?;
                continue;
            }
            if matches!(
                self.policy,
                DispatchPolicy::LiveCoalesce | DispatchPolicy::WebRtcPlay
            ) {
                if let Some(v) = batch.iter().rev().find(|f| is_playable_video(f)) {
                    return Ok(v.clone());
                }
            }
            return Ok(batch.into_iter().last().unwrap());
        }
    }

    /// Jump to live edge after falling behind (replaces drain_broadcast_lag).
    pub fn recover_lag(&mut self, stream_id: &str, dropped_hint: u64) {
        if dropped_hint > 0 {
            info!(
                "[Dispatch] [{}] recover lag hint={} — snap to live edge",
                stream_id, dropped_hint
            );
        }
        self.snap_to_live_edge();
    }

    async fn ensure_data(&mut self) -> Result<(), DispatchError> {
        if !self.hub.is_empty() && self.cursor <= self.hub.latest_seq() {
            return Ok(());
        }
        if self.wake.changed().await.is_err() {
            return Err(DispatchError::Closed);
        }
        Ok(())
    }

    async fn wait_new_frames(&mut self, deadline: Instant) -> Result<(), ()> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(());
        }
        tokio::select! {
            _ = tokio::time::sleep(remaining) => Err(()),
            r = self.wake.changed() => r.map_err(|_| ()),
        }
    }
}

fn is_playable_idr(frame: &MediaFrame) -> bool {
    is_playable_video(frame) && (frame.is_keyframe || is_keyframe_annex_b(&frame.data))
}

/// Coalesce video to latest in batch; keep audio in order.
pub fn coalesce_flv_batch(frames: Vec<MediaFrame>) -> Vec<MediaFrame> {
    let mut out = Vec::new();
    let mut last_video: Option<MediaFrame> = None;
    for f in frames {
        if is_playable_video(&f) {
            last_video = Some(f);
        } else {
            out.push(f);
        }
    }
    if let Some(v) = last_video {
        out.push(v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn coalesce_keeps_latest_video() {
        let frames = vec![
            MediaFrame::new(
                "s".into(),
                0,
                1,
                Bytes::from_static(b"v1"),
                false,
                CodecType::H264,
            ),
            MediaFrame::new(
                "s".into(),
                1,
                2,
                Bytes::from_static(b"a1"),
                false,
                CodecType::AAC,
            ),
            MediaFrame::new(
                "s".into(),
                0,
                3,
                Bytes::from_static(b"v2"),
                false,
                CodecType::H264,
            ),
        ];
        let out = coalesce_flv_batch(frames);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].codec, CodecType::AAC);
        assert_eq!(&out[1].data[..], b"v2");
    }

    fn annex_b_idr() -> Bytes {
        Bytes::from(vec![0, 0, 0, 1, 0x65, 0x88, 0x84, 0])
    }

    fn annex_b_p() -> Bytes {
        Bytes::from(vec![0, 0, 0, 1, 0x41, 0x9a, 0])
    }

    fn publish_gop(hub: &StreamHub, gop: u64, base: u64) {
        let ticks = 3600u64;
        for f in 0..25u64 {
            let ts = base + (gop * 25 + f) * ticks;
            let key = f == 0;
            hub.publish(MediaFrame::new(
                hub.stream_id.clone(),
                0,
                ts,
                if key { annex_b_idr() } else { annex_b_p() },
                key,
                CodecType::H264,
            ));
        }
    }

    #[tokio::test]
    async fn sequential_from_idr_delivers_all_buffered_frames_without_lag_snap() {
        let hub = StreamHub::new("s");
        let base = 2_648_000_000u64;
        let mut reader = DispatchReader::new(hub.clone(), DispatchPolicy::SequentialFromIdr);
        reader.cursor = 0;

        for gop in 0..2 {
            publish_gop(&hub, gop, base);
        }

        let mut delivered = 0usize;
        while delivered < 50 {
            let batch = reader.recv_batch().await.unwrap();
            assert!(
                !reader.take_muxer_resync(),
                "must not snap while requested frames are still buffered"
            );
            if batch.is_empty() {
                break;
            }
            delivered += batch.len();
        }

        assert_eq!(
            delivered, 50,
            "SequentialFromIdr should deliver every buffered frame"
        );
    }

    #[tokio::test]
    async fn sequential_from_idr_waits_when_ring_is_empty() {
        let hub = StreamHub::new("s");
        let mut reader = DispatchReader::new(hub.clone(), DispatchPolicy::SequentialFromIdr);

        let timed_out = tokio::time::timeout(Duration::from_millis(20), reader.recv_batch()).await;
        assert!(
            timed_out.is_err(),
            "empty ring should wait for a frame instead of returning an empty batch"
        );

        publish_gop(&hub, 0, 2_648_000_000);
        let batch = tokio::time::timeout(Duration::from_millis(100), reader.recv_batch())
            .await
            .expect("reader should wake after first publish")
            .expect("reader should stay open");
        assert!(!batch.is_empty());
    }

    #[tokio::test]
    async fn sequential_from_idr_does_not_snap_large_lag_when_ring_has_data() {
        let hub = StreamHub::new("s");
        let base = 2_648_000_000u64;
        for gop in 0..4 {
            publish_gop(&hub, gop, base);
        }

        let mut reader = DispatchReader::new(hub.clone(), DispatchPolicy::SequentialFromIdr);
        reader.cursor = 0;

        let batch = reader.recv_batch().await.unwrap();
        assert!(
            batch.len() == 96,
            "large buffered lag should be drained sequentially in capped batches, got {}",
            batch.len()
        );
        assert!(
            !reader.take_muxer_resync(),
            "large buffered lag should not flag muxer resync"
        );
    }

    #[tokio::test]
    async fn live_sequential_preserves_buffered_frames_after_subscribe() {
        let hub = StreamHub::new("s");
        let base = 2_648_000_000u64;
        publish_gop(&hub, 0, base);

        let mut reader = DispatchReader::new(hub.clone(), DispatchPolicy::LiveSequential);
        publish_gop(&hub, 1, base);

        let batch = reader.recv_batch().await.unwrap();
        assert!(
            batch.len() > 1,
            "LiveSequential should drain accumulated live frames instead of snapping to edge"
        );
        assert!(
            !reader.take_live_snap(),
            "LiveSequential should not report a live snap when frames are still buffered"
        );
    }

    #[tokio::test]
    async fn prime_from_idr_waits_for_fresh_idr_when_existing_idr_is_stale() {
        let hub = StreamHub::new("s");
        let base = 2_648_000_000u64;
        hub.publish(MediaFrame::new(
            "s".into(),
            0,
            base,
            annex_b_idr(),
            true,
            CodecType::H264,
        ));
        for i in 1..=40u64 {
            hub.publish(MediaFrame::new(
                "s".into(),
                0,
                base + i * 3600,
                annex_b_p(),
                false,
                CodecType::H264,
            ));
        }

        let hub_for_publish = hub.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            hub_for_publish.publish(MediaFrame::new(
                "s".into(),
                0,
                base + 41 * 3600,
                annex_b_idr(),
                true,
                CodecType::H264,
            ));
        });

        let manager = StreamManager::new();
        let mut reader = DispatchReader::new(hub.clone(), DispatchPolicy::WebRtcPlay);
        let idr = reader
            .prime_from_idr(&manager, "s")
            .await
            .expect("fresh IDR should be primed");

        assert_eq!(idr.timestamp, base + 41 * 3600);
        assert_eq!(
            reader.cursor(),
            41,
            "cursor should start at the fresh IDR seq"
        );
    }

    #[tokio::test]
    async fn webrtc_play_preserves_frames_under_lag_threshold() {
        let hub = StreamHub::new("s");
        let base = 2_648_000_000u64;
        publish_gop(&hub, 0, base);

        let mut reader = DispatchReader::new(hub.clone(), DispatchPolicy::WebRtcPlay);
        reader.cursor = 0;

        let mut delivered = 0usize;
        for _ in 0..20 {
            let batch = reader.recv_batch().await.unwrap();
            assert!(
                !reader.take_live_snap(),
                "WebRTC play must not snap when lag is below threshold"
            );
            if batch.is_empty() {
                break;
            }
            delivered += batch.len();
        }

        assert_eq!(
            delivered, 20,
            "WebRTC play should drain buffered frames sequentially without per-frame snap"
        );
    }

    #[tokio::test]
    async fn webrtc_play_snaps_when_lag_exceeds_threshold() {
        let hub = StreamHub::new("s");
        let base = 2_648_000_000u64;
        for gop in 0..4 {
            publish_gop(&hub, gop, base);
        }

        let mut reader = DispatchReader::new(hub.clone(), DispatchPolicy::WebRtcPlay);
        reader.cursor = 0;

        let batch = reader.recv_batch().await.unwrap();
        assert!(
            batch.len() < 100,
            "WebRTC play should snap when lag > {WEBRTC_LAG_SNAP_THRESHOLD}, delivered {}",
            batch.len()
        );
        assert!(reader.take_live_snap(), "large lag should flag live snap");
    }
}
