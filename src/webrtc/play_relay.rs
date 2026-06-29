//! Per-stream play relay lifecycle (cancel on stop / restart).

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tracing::info;

struct PlayRelayCtrl {
    stop: watch::Sender<bool>,
    abort: Option<AbortHandle>,
}

fn relays() -> &'static Mutex<HashMap<String, PlayRelayCtrl>> {
    static MAP: OnceLock<Mutex<HashMap<String, PlayRelayCtrl>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Stop an active play relay. Returns true if a relay was running (replay).
pub fn cancel_play_relay(stream_id: &str) -> bool {
    if let Some(ctrl) = relays().lock().remove(stream_id) {
        let _ = ctrl.stop.send(true);
        if let Some(abort) = ctrl.abort {
            abort.abort();
        }
        info!("[WebRTC] Cancelled play relay stream='{}'", stream_id);
        true
    } else {
        false
    }
}

/// Register a new play relay; cancels any previous relay for the same stream.
pub fn register_play_relay(stream_id: &str) -> (watch::Receiver<bool>, bool) {
    let was_active = cancel_play_relay(stream_id);
    let (tx, rx) = watch::channel(false);
    relays().lock().insert(
        stream_id.to_string(),
        PlayRelayCtrl {
            stop: tx,
            abort: None,
        },
    );
    (rx, was_active)
}

pub fn attach_relay_abort_handle(stream_id: &str, abort: AbortHandle) {
    if let Some(ctrl) = relays().lock().get_mut(stream_id) {
        ctrl.abort = Some(abort);
    }
}

pub fn unregister_play_relay(stream_id: &str) {
    relays().lock().remove(stream_id);
}
