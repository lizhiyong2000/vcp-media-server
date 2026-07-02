//! Routes server signals (e.g. need_keyframe) to the publish WebSocket session per stream.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;
use tracing::{info, warn};
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;

use super::signaling::ServerSignal;

struct Entry {
    tx: Option<mpsc::UnboundedSender<ServerSignal>>,
    pli_tx: Option<mpsc::UnboundedSender<()>>,
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
    });
    entry.tx = Some(tx);
    info!(
        "[WebRTC] Registered publish signaling stream='{}'",
        stream_id
    );
}

pub fn register_publish_pli(stream_id: &str, pc: Arc<RTCPeerConnection>, media_ssrc: u32) {
    let (tx, mut rx) = mpsc::unbounded_channel::<()>();
    {
        let mut map = map().lock();
        let entry = map.entry(stream_id.to_string()).or_insert_with(|| Entry {
            tx: None,
            pli_tx: None,
        });
        entry.pli_tx = Some(tx);
    }

    let stream_id = stream_id.to_string();
    let task_stream_id = stream_id.clone();
    tokio::spawn(async move {
        let mut seq = 0u64;
        while rx.recv().await.is_some() {
            seq += 1;
            let pkt = PictureLossIndication {
                sender_ssrc: 0,
                media_ssrc,
            };
            match pc.write_rtcp(&[Box::new(pkt)]).await {
                Ok(_) => info!(
                    "[WebRTC] Sent RTCP PLI #{} stream='{}' media_ssrc={}",
                    seq, task_stream_id, media_ssrc
                ),
                Err(e) => warn!(
                    "[WebRTC] Failed to send RTCP PLI stream='{}' media_ssrc={}: {}",
                    task_stream_id, media_ssrc, e
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
    let (tx, pli_tx) = map()
        .lock()
        .get(stream_id)
        .map(|e| (e.tx.clone(), e.pli_tx.clone()))
        .unwrap_or((None, None));
    let mut requested = false;

    if let Some(pli_tx) = pli_tx {
        if pli_tx.send(()).is_ok() {
            info!("[WebRTC] Queued RTCP PLI stream='{}'", stream_id);
            requested = true;
        } else {
            warn!("[WebRTC] Failed to queue RTCP PLI stream='{}'", stream_id);
        }
    }

    let Some(tx) = tx else {
        warn!(
            "[WebRTC] No publish signaling for stream='{}' — is the publisher page connected?",
            stream_id
        );
        return requested;
    };
    if tx.send(ServerSignal::NeedKeyframe).is_ok() {
        info!(
            "[WebRTC] Forwarded need_keyframe to publisher stream='{}'",
            stream_id
        );
        true
    } else {
        warn!(
            "[WebRTC] Failed to forward need_keyframe stream='{}'",
            stream_id
        );
        requested
    }
}
