//! Per-session play relay lifecycle (multiple concurrent players per stream).

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tracing::info;
use uuid::Uuid;

struct PlayRelayCtrl {
    stream_id: String,
    stop: watch::Sender<bool>,
    abort: Option<AbortHandle>,
}

fn relays() -> &'static Mutex<HashMap<String, PlayRelayCtrl>> {
    static MAP: OnceLock<Mutex<HashMap<String, PlayRelayCtrl>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

fn count_for_stream(stream_id: &str) -> usize {
    relays()
        .lock()
        .values()
        .filter(|c| c.stream_id == stream_id)
        .count()
}

/// Cooperative stop: set the watch flag so the relay loop exits on its own.
pub fn signal_play_relay_stop(relay_id: &str) -> bool {
    if let Some(ctrl) = relays().lock().get(relay_id) {
        let _ = ctrl.stop.send(true);
        true
    } else {
        false
    }
}

/// Stop one play relay by session id. Returns true if it was running.
pub fn cancel_play_relay(relay_id: &str) -> bool {
    let removed = relays().lock().remove(relay_id);
    let Some(ctrl) = removed else {
        return false;
    };

    let stream_id = ctrl.stream_id.clone();
    let _ = ctrl.stop.send(true);
    let abort = ctrl.abort;

    if let Some(abort) = abort {
        abort.abort();
    }
    info!(
        "[WebRTC] Cancelled play relay id='{}' stream='{}' remaining={}",
        relay_id,
        stream_id,
        count_for_stream(&stream_id)
    );
    true
}

/// Register a new play relay; does not cancel other players on the same stream.
pub fn register_play_relay(stream_id: &str) -> (String, watch::Receiver<bool>, usize) {
    let relay_id = Uuid::new_v4().to_string();
    let (tx, rx) = watch::channel(false);
    relays().lock().insert(
        relay_id.clone(),
        PlayRelayCtrl {
            stream_id: stream_id.to_string(),
            stop: tx,
            abort: None,
        },
    );
    let active = count_for_stream(stream_id);
    info!(
        "[WebRTC] Registered play relay id='{}' stream='{}' active_players={}",
        relay_id, stream_id, active
    );
    (relay_id, rx, active)
}

pub fn attach_relay_abort_handle(relay_id: &str, abort: AbortHandle) {
    if let Some(ctrl) = relays().lock().get_mut(relay_id) {
        ctrl.abort = Some(abort);
    }
}

pub fn unregister_play_relay(relay_id: &str) {
    if relays().lock().remove(relay_id).is_some() {
        info!("[WebRTC] Unregistered play relay id='{}'", relay_id);
    }
}

/// Stop all play relays for a stream (e.g. when the publisher disconnects).
pub fn stop_play_relays_for_stream(stream_id: &str) -> usize {
    let relay_ids: Vec<String> = relays()
        .lock()
        .iter()
        .filter(|(_, ctrl)| ctrl.stream_id == stream_id)
        .map(|(id, _)| id.clone())
        .collect();
    let count = relay_ids.len();
    for id in relay_ids {
        signal_play_relay_stop(&id);
    }
    if count > 0 {
        info!(
            "[WebRTC] Signalled stop for {} play relay(s) on stream='{}'",
            count, stream_id
        );
    }
    count
}
