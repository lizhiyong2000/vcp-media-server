//! GOP-aware bounded frame ring for per-stream media cache.

use std::collections::VecDeque;

use bytes::Bytes;

use super::{is_keyframe_annex_b, CodecType, MediaFrame, StreamId, TrackId};

pub const DEFAULT_CAPACITY_FRAMES: usize = 512;
pub const DEFAULT_CAPACITY_BYTES: usize = 32 * 1024 * 1024;
const MIN_GOPS_RETAINED: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapMode {
    LiveEdge,
    LatestIdr,
}

#[derive(Debug, Clone)]
pub struct StoredFrame {
    pub seq: u64,
    pub stream_id: StreamId,
    pub track_id: TrackId,
    pub timestamp: u64,
    pub clock_rate: Option<u32>,
    pub codec: CodecType,
    pub is_keyframe: bool,
    pub data: Bytes,
}

impl StoredFrame {
    pub fn to_media_frame(&self) -> MediaFrame {
        MediaFrame::new(
            self.stream_id.clone(),
            self.track_id,
            self.timestamp,
            self.data.clone(),
            self.is_keyframe,
            self.codec,
        )
        .with_optional_clock_rate(self.clock_rate)
    }
}

pub struct FrameRing {
    capacity_frames: usize,
    capacity_bytes: usize,
    slots: VecDeque<StoredFrame>,
    idr_seqs: VecDeque<u64>,
    gop_starts: VecDeque<u64>,
    write_seq: u64,
    bytes_used: usize,
}

impl FrameRing {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY_FRAMES, DEFAULT_CAPACITY_BYTES)
    }

    pub fn with_capacity(capacity_frames: usize, capacity_bytes: usize) -> Self {
        Self {
            capacity_frames,
            capacity_bytes,
            slots: VecDeque::new(),
            idr_seqs: VecDeque::new(),
            gop_starts: VecDeque::new(),
            write_seq: 0,
            bytes_used: 0,
        }
    }

    pub fn push(&mut self, frame: MediaFrame) -> u64 {
        let seq = self.write_seq;
        let key = is_video_keyframe(&frame);
        if key {
            self.gop_starts.push_back(seq);
            self.idr_seqs.push_back(seq);
        }

        let bytes = frame.data.len();
        let stored = StoredFrame {
            seq,
            stream_id: frame.stream_id,
            track_id: frame.track_id,
            timestamp: frame.timestamp,
            clock_rate: frame.clock_rate,
            codec: frame.codec,
            is_keyframe: key,
            data: frame.data,
        };
        self.bytes_used += bytes;
        self.slots.push_back(stored);
        self.write_seq += 1;
        self.evict_if_needed();
        seq
    }

    pub fn get(&self, seq: u64) -> Option<&StoredFrame> {
        self.slots.iter().find(|f| f.seq == seq)
    }

    pub fn latest_seq(&self) -> u64 {
        self.write_seq.saturating_sub(1)
    }

    pub fn oldest_seq(&self) -> Option<u64> {
        self.slots.front().map(|f| f.seq)
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn bytes_used(&self) -> usize {
        self.bytes_used
    }

    pub fn latest_idr_seq(&self) -> Option<u64> {
        self.idr_seqs.back().copied()
    }

    pub fn iter_idr_seqs_rev(&self) -> impl Iterator<Item = u64> + '_ {
        self.idr_seqs.iter().rev().copied()
    }

    pub fn latest_idr_frame(&self) -> Option<MediaFrame> {
        let seq = self.latest_idr_seq()?;
        self.get(seq).map(|f| f.to_media_frame())
    }

    pub fn snap(&self, mode: SnapMode) -> u64 {
        match mode {
            SnapMode::LiveEdge => self.latest_seq(),
            SnapMode::LatestIdr => self.latest_idr_seq().unwrap_or_else(|| self.latest_seq()),
        }
    }

    pub fn frames_from(&self, from_seq: u64, to_seq: u64) -> Vec<MediaFrame> {
        let mut out = Vec::new();
        for f in &self.slots {
            if f.seq >= from_seq && f.seq <= to_seq {
                out.push(f.to_media_frame());
            }
        }
        out
    }

    pub fn find_frame_rev(
        &self,
        from_seq: u64,
        to_seq: u64,
        mut predicate: impl FnMut(&StoredFrame) -> bool,
    ) -> Option<MediaFrame> {
        for f in self.slots.iter().rev() {
            if f.seq < from_seq {
                break;
            }
            if f.seq > to_seq {
                continue;
            }
            if predicate(f) {
                return Some(f.to_media_frame());
            }
        }
        None
    }

    fn needs_evict(&self) -> bool {
        self.slots.len() > self.capacity_frames || self.bytes_used > self.capacity_bytes
    }

    fn evict_if_needed(&mut self) {
        while self.needs_evict() {
            if self.gop_starts.len() > MIN_GOPS_RETAINED {
                self.evict_oldest_gop();
            } else if self.slots.len() > 1 {
                let before_len = self.slots.len();
                let before_bytes = self.bytes_used;
                self.evict_front_until_keyframe_or_empty();
                if self.slots.len() == before_len && self.bytes_used == before_bytes {
                    self.evict_oldest_non_key_after_front();
                }
            } else {
                break;
            }
        }
    }

    fn evict_oldest_gop(&mut self) {
        let Some(start) = self.gop_starts.pop_front() else {
            return;
        };
        let end = self.gop_starts.front().copied().unwrap_or(self.write_seq);
        self.remove_seq_range(start, end);
        self.idr_seqs.retain(|s| *s != start);
    }

    fn evict_front_until_keyframe_or_empty(&mut self) {
        while let Some(front) = self.slots.front() {
            if front.is_keyframe {
                break;
            }
            if let Some(f) = self.slots.pop_front() {
                self.bytes_used = self.bytes_used.saturating_sub(f.data.len());
            }
        }
    }

    fn evict_oldest_non_key_after_front(&mut self) {
        if self.slots.len() <= 1 {
            return;
        }
        if let Some(f) = self.slots.remove(1) {
            self.bytes_used = self.bytes_used.saturating_sub(f.data.len());
        }
    }

    fn remove_seq_range(&mut self, start: u64, end: u64) {
        self.slots.retain(|f| {
            let drop = f.seq >= start && f.seq < end;
            if drop {
                self.bytes_used = self.bytes_used.saturating_sub(f.data.len());
            }
            !drop
        });
    }
}

impl Default for FrameRing {
    fn default() -> Self {
        Self::new()
    }
}

pub fn is_video_keyframe(frame: &MediaFrame) -> bool {
    if !matches!(frame.codec, CodecType::H264 | CodecType::H265) {
        return false;
    }
    if frame.codec == CodecType::H265 {
        return is_keyframe_annex_b(frame.codec, &frame.data);
    }
    if frame.is_keyframe {
        return true;
    }
    is_keyframe_annex_b(frame.codec, &frame.data)
}

pub fn is_playable_video(frame: &MediaFrame) -> bool {
    matches!(frame.codec, CodecType::H264 | CodecType::H265) && !frame.data.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn video_frame(seq_ts: u64, key: bool) -> MediaFrame {
        let nal = if key { 0x65u8 } else { 0x41u8 };
        MediaFrame::new(
            "s1".into(),
            0,
            seq_ts,
            Bytes::from(vec![0, 0, 0, 1, nal]),
            key,
            CodecType::H264,
        )
    }

    fn hevc_idr_frame(seq_ts: u64, key: bool) -> MediaFrame {
        MediaFrame::new(
            "s1".into(),
            0,
            seq_ts,
            Bytes::from(vec![0, 0, 0, 1, 0x26, 0x01, 0xaf]),
            key,
            CodecType::H265,
        )
    }

    fn hevc_cra_frame(seq_ts: u64, key: bool) -> MediaFrame {
        MediaFrame::new(
            "s1".into(),
            0,
            seq_ts,
            Bytes::from(vec![0, 0, 0, 1, 0x2a, 0x01, 0xaf]),
            key,
            CodecType::H265,
        )
    }

    fn audio_frame(seq_ts: u64) -> MediaFrame {
        MediaFrame::new(
            "s1".into(),
            1,
            seq_ts,
            Bytes::from(vec![0x12, 0x10, 0x56]),
            false,
            CodecType::AAC,
        )
    }

    #[test]
    fn push_assigns_monotonic_seq() {
        let mut ring = FrameRing::new();
        assert_eq!(ring.push(video_frame(1, true)), 0);
        assert_eq!(ring.push(video_frame(2, false)), 1);
        assert_eq!(ring.latest_seq(), 1);
    }

    #[test]
    fn snap_latest_idr() {
        let mut ring = FrameRing::new();
        ring.push(video_frame(1, true));
        ring.push(video_frame(2, false));
        ring.push(video_frame(3, true));
        ring.push(video_frame(4, false));
        assert_eq!(ring.snap(SnapMode::LatestIdr), 2);
    }

    #[test]
    fn h265_idr_is_detected_even_when_frame_flag_is_missing() {
        let mut ring = FrameRing::new();
        ring.push(video_frame(1, true));
        let seq = ring.push(hevc_idr_frame(2, false));

        assert_eq!(ring.latest_idr_seq(), Some(seq));
    }

    #[test]
    fn h265_cra_is_treated_as_keyframe_even_when_only_bitstream_indicates_it() {
        let mut ring = FrameRing::new();
        ring.push(video_frame(1, true));
        let cra_seq = ring.push(hevc_cra_frame(2, false));

        assert_eq!(ring.latest_idr_seq(), Some(cra_seq));
    }

    #[test]
    fn evict_whole_gop_not_mid_gop() {
        let mut ring = FrameRing::with_capacity(4, 1024);
        ring.push(video_frame(1, true));
        ring.push(video_frame(2, false));
        ring.push(video_frame(3, false));
        ring.push(video_frame(4, true)); // triggers evict of first GOP
        ring.push(video_frame(5, false));
        assert!(ring.get(0).is_none());
        assert!(ring.get(1).is_none());
        assert!(ring.get(2).is_none());
        assert!(ring.get(3).is_some());
        assert!(ring.get(4).is_some());
    }

    #[test]
    fn evict_makes_progress_when_single_gop_exceeds_capacity_with_audio() {
        let mut ring = FrameRing::with_capacity(4, 1024);
        let idr_seq = ring.push(video_frame(1, true));

        for i in 0..20 {
            ring.push(audio_frame(2 + i));
        }

        assert!(ring.len() <= 4);
        assert!(ring.get(idr_seq).is_some());
        assert_eq!(ring.latest_idr_seq(), Some(idr_seq));
    }
}
