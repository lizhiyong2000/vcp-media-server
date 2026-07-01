//! Per-stream hub: FrameRing storage + seq notification.

use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::watch;

use super::frame_ring::{FrameRing, SnapMode, StoredFrame};
use super::{MediaFrame, StreamId};

pub struct StreamHub {
    pub stream_id: StreamId,
    ring: RwLock<FrameRing>,
    seq_tx: watch::Sender<u64>,
}

impl StreamHub {
    pub fn new(stream_id: &str) -> Arc<Self> {
        let (seq_tx, _) = watch::channel(0u64);
        Arc::new(Self {
            stream_id: stream_id.to_string(),
            ring: RwLock::new(FrameRing::new()),
            seq_tx,
        })
    }

    pub fn publish(&self, frame: MediaFrame) -> u64 {
        let seq = self.ring.write().push(frame);
        let _ = self.seq_tx.send(seq);
        seq
    }

    pub fn subscribe_notify(&self) -> watch::Receiver<u64> {
        self.seq_tx.subscribe()
    }

    pub fn get(&self, seq: u64) -> Option<MediaFrame> {
        self.ring.read().get(seq).map(|f| f.to_media_frame())
    }

    pub fn get_stored(&self, seq: u64) -> Option<StoredFrame> {
        self.ring.read().get(seq).cloned()
    }

    pub fn latest_seq(&self) -> u64 {
        self.ring.read().latest_seq()
    }

    pub fn snap(&self, mode: SnapMode) -> u64 {
        self.ring.read().snap(mode)
    }

    pub fn latest_idr_frame(&self) -> Option<MediaFrame> {
        self.ring.read().latest_idr_frame()
    }

    pub fn latest_idr_bytes(&self) -> Option<(Vec<u8>, u64)> {
        self.ring.read().latest_idr_frame().map(|f| {
            (f.data.to_vec(), f.timestamp)
        })
    }

    pub fn frames_from(&self, from_seq: u64, to_seq: u64) -> Vec<MediaFrame> {
        self.ring.read().frames_from(from_seq, to_seq)
    }

    pub fn oldest_seq(&self) -> Option<u64> {
        self.ring.read().oldest_seq()
    }

    pub fn reset(&self) {
        *self.ring.write() = FrameRing::new();
        let _ = self.seq_tx.send(0);
    }
}
