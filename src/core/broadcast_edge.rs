//! Keep broadcast subscribers at the live edge when they fall behind.

use tokio::sync::broadcast;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use super::{CodecType, MediaFrame};

/// Drain all frames currently queued for this receiver (after `RecvError::Lagged`).
pub fn drain_broadcast_lag(rx: &mut broadcast::Receiver<MediaFrame>) -> u64 {
    let mut dropped = 0u64;
    loop {
        match rx.try_recv() {
            Ok(_) => dropped += 1,
            Err(TryRecvError::Lagged(n)) => dropped += n,
            Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
        }
    }
    dropped
}

pub fn is_playable_video(frame: &MediaFrame) -> bool {
    matches!(frame.codec, CodecType::H264 | CodecType::H265) && !frame.data.is_empty()
}

/// Block for one frame, then coalesce any burst to the latest playable video frame.
pub async fn recv_coalesced_video(
    rx: &mut broadcast::Receiver<MediaFrame>,
) -> Result<(MediaFrame, u64), RecvError> {
    let mut latest = rx.recv().await?;
    let mut coalesced = 0u64;
    loop {
        match rx.try_recv() {
            Ok(next) => {
                if is_playable_video(&next) {
                    coalesced += 1;
                    latest = next;
                }
            }
            Err(TryRecvError::Lagged(n)) => return Err(RecvError::Lagged(n)),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Closed) => return Err(RecvError::Closed),
        }
    }
    Ok((latest, coalesced))
}

/// Block for one frame, then drain any burst: keep audio in order, coalesce video to latest.
pub async fn recv_flv_batch(
    rx: &mut broadcast::Receiver<MediaFrame>,
) -> Result<Vec<MediaFrame>, RecvError> {
    let first = rx.recv().await?;
    let mut frames = vec![first];
    loop {
        match rx.try_recv() {
            Ok(f) => frames.push(f),
            Err(TryRecvError::Lagged(n)) => return Err(RecvError::Lagged(n)),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Closed) => return Err(RecvError::Closed),
        }
    }

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
    Ok(out)
}
