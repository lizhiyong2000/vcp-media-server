//! Routes server signals (e.g. need_keyframe) to the publish WebSocket session per stream.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::signaling::ServerSignal;

struct Entry {
    tx: mpsc::UnboundedSender<ServerSignal>,
}

fn map() -> &'static Mutex<HashMap<String, Entry>> {
    static MAP: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register the publish client's signaling channel for a stream.
pub fn register_publish_signaling(stream_id: &str, tx: mpsc::UnboundedSender<ServerSignal>) {
    map().lock().insert(stream_id.to_string(), Entry { tx });
    info!(
        "[WebRTC] Registered publish signaling stream='{}'",
        stream_id
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
    let tx = map().lock().get(stream_id).map(|e| e.tx.clone());
    let Some(tx) = tx else {
        warn!(
            "[WebRTC] No publish signaling for stream='{}' — is the publisher page connected?",
            stream_id
        );
        return false;
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
        false
    }
}
