//! Stream helper functions for integration tests.

use std::sync::Arc;
use vcp_media_server::core::{CodecType, StreamManager, StreamProtocol, StreamSourceMode, Track};

use super::{ports, TestCodec};

/// Setup a test stream with default tracks.
pub fn setup_test_stream(
    manager: &Arc<StreamManager>,
    stream_id: &str,
    codec: TestCodec,
) -> Vec<Track> {
    manager.create_stream(
        stream_id,
        StreamSourceMode::Push,
        StreamProtocol::Unknown,
        None,
    );

    let tracks = match codec {
        TestCodec::H264 => vec![
            Track::new(0, CodecType::H264, 96, 90_000),
            Track::new(1, CodecType::AAC, 97, 44_100),
        ],
        TestCodec::H265 => vec![
            Track::new(0, CodecType::H265, 98, 90_000),
            Track::new(1, CodecType::AAC, 97, 44_100),
        ],
    };

    manager.set_stream_tracks(stream_id, tracks.clone());
    manager.ensure_stream_hub(stream_id);

    tracks
}

/// Cleanup test streams by removing them from the manager.
pub fn cleanup_test_streams(manager: &Arc<StreamManager>, stream_ids: &[&str]) {
    for stream_id in stream_ids {
        let _ = manager.remove_stream(&stream_id.to_string());
    }
}

/// Get a unique test stream ID with timestamp.
pub fn unique_stream_id(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    format!("{}_{}", prefix, ts)
}

/// Check if stream is active (publishing or has published data).
pub fn is_stream_active(manager: &Arc<StreamManager>, stream_id: &str) -> bool {
    if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
        if let Some(hub) = manager.get_hub(stream_id) {
            return hub.latest_seq() > 0;
        }
    }
    false
}

/// Wait for stream to become active with timeout.
pub async fn wait_for_stream_active(
    manager: &Arc<StreamManager>,
    stream_id: &str,
    timeout_secs: u64,
) -> bool {
    use std::time::Duration;
    use tokio::time::timeout;

    let check_stream = async {
        while !is_stream_active(manager, stream_id) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        true
    };

    timeout(Duration::from_secs(timeout_secs), check_stream)
        .await
        .unwrap_or(false)
}

/// Get available port for testing.
pub fn get_test_port() -> u16 {
    ports::random_test_port()
}
