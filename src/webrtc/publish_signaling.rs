//! Routes server signals (e.g. need_keyframe) to the publish WebSocket session per stream.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{info, warn};
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;

use super::signaling::ServerSignal;

struct Entry {
    tx: Option<mpsc::UnboundedSender<ServerSignal>>,
    pli_tx: Option<mpsc::UnboundedSender<u64>>,
    last_request: Option<KeyframeRequestRecord>,
}

#[derive(Clone, Copy)]
struct KeyframeRequestRecord {
    id: u64,
    at: Instant,
}

fn map() -> &'static Mutex<HashMap<String, Entry>> {
    static MAP: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register the publish client's signaling channel for a stream.
pub fn register_publish_signaling(stream_id: &str, tx: mpsc::UnboundedSender<ServerSignal>) {
    let mut map = map().lock();
    let entry = map.entry(stream_id.to_string()).or_insert_with(|| Entry {
        tx: None,
        pli_tx: None,
        last_request: None,
    });
    entry.tx = Some(tx);
    info!(
        "[WebRTC] Registered publish signaling stream='{}'",
        stream_id
    );
}

pub fn register_publish_pli(stream_id: &str, pc: Arc<RTCPeerConnection>, media_ssrc: u32) {
    let (tx, mut rx) = mpsc::unbounded_channel::<u64>();
    {
        let mut map = map().lock();
        let entry = map.entry(stream_id.to_string()).or_insert_with(|| Entry {
            tx: None,
            pli_tx: None,
            last_request: None,
        });
        entry.pli_tx = Some(tx);
    }

    let stream_id = stream_id.to_string();
    let task_stream_id = stream_id.clone();
    tokio::spawn(async move {
        let mut seq = 0u64;
        while let Some(request_id) = rx.recv().await {
            seq += 1;
            let pkt = PictureLossIndication {
                sender_ssrc: 0,
                media_ssrc,
            };
            match pc.write_rtcp(&[Box::new(pkt)]).await {
                Ok(_) => info!(
                    "[WebRTC] keyframe_request id={} sent RTCP PLI seq={} stream='{}' media_ssrc={}",
                    request_id, seq, task_stream_id, media_ssrc
                ),
                Err(e) => warn!(
                    "[WebRTC] keyframe_request id={} failed to send RTCP PLI stream='{}' media_ssrc={}: {}",
                    request_id, task_stream_id, media_ssrc, e
                ),
            }
        }
        info!(
            "[WebRTC] Publish PLI task ended stream='{}'",
            task_stream_id
        );
    });

    info!(
        "[WebRTC] Registered publish PLI stream='{}' media_ssrc={}",
        stream_id, media_ssrc
    );
}

pub fn unregister_publish_signaling(stream_id: &str) {
    if map().lock().remove(stream_id).is_some() {
        info!(
            "[WebRTC] Unregistered publish signaling stream='{}'",
            stream_id
        );
    }
}

/// Ask the publisher browser (possibly another tab) to emit an IDR.
pub fn request_publisher_keyframe(stream_id: &str) -> bool {
    static REQUEST_ID: AtomicU64 = AtomicU64::new(1);
    let request_id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let started_at = Instant::now();
    let (tx, pli_tx) = {
        let mut map = map().lock();
        let entry = map.entry(stream_id.to_string()).or_insert_with(|| Entry {
            tx: None,
            pli_tx: None,
            last_request: None,
        });
        entry.last_request = Some(KeyframeRequestRecord {
            id: request_id,
            at: Instant::now(),
        });
        (entry.tx.clone(), entry.pli_tx.clone())
    };
    let mut pli_queued = false;
    let mut signaling_sent = false;

    info!(
        "[WebRTC] keyframe_request id={} start stream='{}' has_pli={} has_signaling={}",
        request_id,
        stream_id,
        pli_tx.is_some(),
        tx.is_some()
    );

    if let Some(pli_tx) = pli_tx {
        if pli_tx.send(request_id).is_ok() {
            info!(
                "[WebRTC] keyframe_request id={} queued RTCP PLI stream='{}'",
                request_id, stream_id
            );
            pli_queued = true;
        } else {
            warn!(
                "[WebRTC] keyframe_request id={} failed to queue RTCP PLI stream='{}'",
                request_id, stream_id
            );
        }
    }

    let Some(tx) = tx else {
        warn!(
            "[WebRTC] keyframe_request id={} no publish signaling for stream='{}' — is the publisher page connected?",
            request_id, stream_id
        );
        info!(
            "[WebRTC] keyframe_request id={} done stream='{}' requested={} pli_queued={} signaling_sent={} elapsed_ms={}",
            request_id,
            stream_id,
            pli_queued,
            pli_queued,
            signaling_sent,
            started_at.elapsed().as_millis()
        );
        return pli_queued;
    };
    if tx.send(ServerSignal::NeedKeyframe).is_ok() {
        info!(
            "[WebRTC] keyframe_request id={} forwarded need_keyframe to publisher stream='{}'",
            request_id, stream_id
        );
        signaling_sent = true;
    } else {
        warn!(
            "[WebRTC] keyframe_request id={} failed to forward need_keyframe stream='{}'",
            request_id, stream_id
        );
    }
    let requested = pli_queued || signaling_sent;
    info!(
        "[WebRTC] keyframe_request id={} done stream='{}' requested={} pli_queued={} signaling_sent={} elapsed_ms={}",
        request_id,
        stream_id,
        requested,
        pli_queued,
        signaling_sent,
        started_at.elapsed().as_millis()
    );
    requested
}

pub fn latest_keyframe_request_age_ms(stream_id: &str) -> Option<(u64, u128)> {
    map()
        .lock()
        .get(stream_id)
        .and_then(|entry| entry.last_request)
        .map(|record| (record.id, record.at.elapsed().as_millis()))
}
