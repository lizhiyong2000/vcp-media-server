use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use webrtc::api::media_engine::MIME_TYPE_H264;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

use crate::core::{CodecType, MediaFrame, StreamManager};
use super::h264_util::{
    contains_idr_nalu, contains_sps_or_pps_nalu, describe_annex_b, ensure_annex_b, extract_sps_pps,
    is_parameter_set_only, is_rtp_timestamp_after, is_rtp_timestamp_before, iter_annex_b_nal_ranges,
    looks_like_h265_misread_as_h264,
};
use super::outbound_h264::{annex_b_with_config, OutboundH264Track};
use super::peer::{new_peer_connection, wire_pc_debug};
use super::publisher::wire_ice_candidates;
use super::play_relay::{attach_relay_abort_handle, register_play_relay, unregister_play_relay};
use super::publish_signaling::request_publisher_keyframe;
use super::sdp_h264::{build_h264_sdp_fmtp, patch_answer_sdp_h264};
use super::signaling::ServerSignal;
use webrtc::api::API;

pub use super::play_relay::cancel_play_relay;

/// Fixed playout interval — keeps WebRTC sample timing stable (25 fps ≈ 40 ms).
const PLAY_FRAME_DURATION: Duration = Duration::from_millis(40);

pub struct PlaySession {
    pub answer_sdp: String,
    pub pc: Arc<RTCPeerConnection>,
}

pub async fn start_play(
    api: Arc<API>,
    manager: Arc<StreamManager>,
    stream_id: String,
    offer_sdp: String,
    ice_tx: mpsc::UnboundedSender<ServerSignal>,
) -> Result<PlaySession> {
    if manager.get_stream(&stream_id).is_none() {
        return Err(anyhow!("Stream '{}' not found", stream_id));
    }

    log_stream_codec_state(&manager, &stream_id, "play-request");

    let h264_fmtp = manager.get_stream(&stream_id.to_string()).and_then(|stream| {
        match (&stream.sps, &stream.pps) {
            (Some(sps), Some(pps)) => Some(build_h264_sdp_fmtp(sps, pps)),
            _ => None,
        }
    });

    let pc = new_peer_connection(&api).await?;
    wire_pc_debug(pc.clone(), "play");

    let mut codec_capability = RTCRtpCodecCapability {
        mime_type: MIME_TYPE_H264.to_owned(),
        clock_rate: 90000,
        ..Default::default()
    };
    if let Some(fmtp) = &h264_fmtp {
        codec_capability.sdp_fmtp_line = fmtp.clone();
        info!(
            "[WebRTC] Play track codec fmtp stream='{}' {}",
            stream_id,
            fmtp
        );
    }

    let video_track = Arc::new(TrackLocalStaticSample::new(
        codec_capability,
        "video".to_owned(),
        stream_id.clone(),
    ));

    let _rtp_sender = pc
        .add_track(Arc::clone(&video_track) as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
        .await?;
    info!("[WebRTC] Play added outbound RTP video track stream='{}'", stream_id);

    wire_ice_candidates(pc.clone(), ice_tx.clone());

    let offer = RTCSessionDescription::offer(offer_sdp)?;
    pc.set_remote_description(offer).await?;
    info!("[WebRTC] Play set remote offer stream='{}'", stream_id);

    let answer = pc.create_answer(None).await?;
    pc.set_local_description(answer.clone()).await?;

    let mut answer_sdp = answer.sdp.clone();
    if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
        if let (Some(sps), Some(pps)) = (&stream.sps, &stream.pps) {
            answer_sdp = patch_answer_sdp_h264(&answer_sdp, sps, pps);
        }
    }

    info!(
        "[WebRTC] Play local answer ready stream='{}' sdp_len={}",
        stream_id,
        answer_sdp.len()
    );

    let outbound = OutboundH264Track::new(video_track);
    let (stop_rx, is_replay) = register_play_relay(&stream_id);
    let manager_clone = manager.clone();
    let sid = stream_id.clone();
    let pc_clone = pc.clone();
    let relay_handle = tokio::spawn(async move {
        let result = relay_stream_to_track(
            manager_clone,
            sid.clone(),
            outbound,
            pc_clone,
            stop_rx,
            is_replay,
        )
        .await;
        if let Err(e) = result {
            error!("[WebRTC] Play relay error stream='{}': {}", sid, e);
        }
    });
    attach_relay_abort_handle(&stream_id, relay_handle.abort_handle());

    info!("[WebRTC] Play session ready for stream '{}'", stream_id);

    Ok(PlaySession {
        answer_sdp,
        pc,
    })
}

fn log_stream_codec_state(manager: &StreamManager, stream_id: &str, phase: &str) {
    if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
        info!(
            "[WebRTC] Stream state [{}] id='{}' status={:?} sps={} pps={}",
            phase,
            stream_id,
            stream.status,
            stream.sps.as_ref().map(|s| s.len()).unwrap_or(0),
            stream.pps.as_ref().map(|p| p.len()).unwrap_or(0)
        );
    } else {
        warn!("[WebRTC] Stream state [{}] id='{}' NOT FOUND", phase, stream_id);
    }
}

async fn wait_for_connected_collecting(
    pc: &Arc<RTCPeerConnection>,
    rx: &mut tokio::sync::broadcast::Receiver<MediaFrame>,
    label: &str,
) -> Result<Vec<MediaFrame>> {
    let mut backlog = Vec::new();
    for attempt in 0..200 {
        collect_available_frames(rx, &mut backlog);

        match pc.connection_state() {
            RTCPeerConnectionState::Connected => {
                collect_available_frames(rx, &mut backlog);
                info!(
                    "[WebRTC] {} peer connection connected (attempt={}) backlog={}",
                    label,
                    attempt,
                    backlog.len()
                );
                return Ok(backlog);
            }
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                return Err(anyhow!(
                    "{} peer connection {:?} before media relay",
                    label,
                    pc.connection_state()
                ));
            }
            _ => {
                if attempt == 0 || attempt % 20 == 0 {
                    debug!(
                        "[WebRTC] {} waiting connected: {:?} (attempt={} backlog={})",
                        label,
                        pc.connection_state(),
                        attempt,
                        backlog.len()
                    );
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    Err(anyhow!("{} timeout waiting for peer connection", label))
}

fn collect_available_frames(
    rx: &mut tokio::sync::broadcast::Receiver<MediaFrame>,
    backlog: &mut Vec<MediaFrame>,
) {
    use tokio::sync::broadcast::error::TryRecvError;

    loop {
        match rx.try_recv() {
            Ok(frame) => backlog.push(frame),
            Err(TryRecvError::Lagged(n)) => {
                warn!("[WebRTC] Play collect lagged {} frames (dropped)", n);
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
        }
    }
}

/// Drain broadcast buffer and return the most recent IDR frame, if any.
fn drain_to_latest_idr(rx: &mut tokio::sync::broadcast::Receiver<MediaFrame>) -> Option<MediaFrame> {
    use tokio::sync::broadcast::error::TryRecvError;

    let mut latest_idr: Option<MediaFrame> = None;
    loop {
        match rx.try_recv() {
            Ok(frame) => {
                if !is_idr_frame(&frame) {
                    continue;
                }
                latest_idr = Some(match latest_idr {
                    None => frame,
                    Some(prev) if is_rtp_timestamp_after(frame.timestamp, prev.timestamp) => frame,
                    Some(prev) => prev,
                });
            }
            Err(TryRecvError::Lagged(_)) => continue,
            Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
        }
    }
    latest_idr
}

fn stream_has_config(manager: &StreamManager, stream_id: &str) -> bool {
    manager
        .get_stream(&stream_id.to_string())
        .map(|s| s.sps.is_some() && s.pps.is_some())
        .unwrap_or(false)
}

async fn relay_stream_to_track(
    manager: Arc<StreamManager>,
    stream_id: String,
    outbound: OutboundH264Track,
    pc: Arc<RTCPeerConnection>,
    mut stop_rx: tokio::sync::watch::Receiver<bool>,
    is_replay: bool,
) -> Result<()> {
    let _guard = RelayCleanup(&stream_id);

    struct RelayCleanup<'a>(&'a str);
    impl Drop for RelayCleanup<'_> {
        fn drop(&mut self) {
            unregister_play_relay(self.0);
        }
    }

    let Some(mut rx) = manager.subscribe(&stream_id) else {
        return Err(anyhow!("No broadcast channel for stream {}", stream_id));
    };

    let backlog = wait_for_connected_collecting(&pc, &mut rx, "play").await?;
    if stop_requested(&mut stop_rx) {
        return Ok(());
    }

    outbound.wait_binding("play").await?;
    log_stream_codec_state(&manager, &stream_id, "relay-start");

    // Use backlog only to learn SPS/PPS — do not replay P-frames (burst causes broadcast lag).
    for frame in &backlog {
        if frame.codec == CodecType::H264 || frame.codec == CodecType::H265 {
            store_live_nalu_config(&manager, &stream_id, &ensure_annex_b(&frame.data));
        }
    }
    let mut config_ready = stream_has_config(&manager, &stream_id);
    if !config_ready {
        warn!(
            "[WebRTC] Play stream='{}' no SPS/PPS yet in backlog ({} frames), wait for live config replay={}",
            stream_id,
            backlog.len(),
            is_replay
        );
    } else {
        info!(
            "[WebRTC] Play stream='{}' config ready from backlog ({} frames), wait for live IDR replay={}",
            stream_id,
            backlog.len(),
            is_replay
        );
    }
    request_publisher_keyframe(&stream_id);

    let mut pending: VecDeque<MediaFrame> = VecDeque::new();
    let mut rtp_sent: u64 = 0;
    let mut received: u64 = 0;
    let mut skipped: u64 = 0;
    let mut streaming = false;
    let mut pace_next = Instant::now();
    let mut last_sent_ts: Option<u64> = None;
    let mut wait_start = Instant::now();
    const KEYFRAME_WAIT: Duration = Duration::from_secs(10);

    info!("[WebRTC] Play relay loop started for stream='{}'", stream_id);

    loop {
        if stop_requested(&mut stop_rx) {
            info!("[WebRTC] Play relay stop requested stream='{}'", stream_id);
            break;
        }
        if pc_connection_ended(&pc) {
            info!(
                "[WebRTC] Play relay ending — PC {:?} stream='{}'",
                pc.connection_state(),
                stream_id
            );
            break;
        }

        let frame = if let Some(f) = pending.pop_front() {
            f
        } else {
            match recv_next_frame(&mut rx, &mut stop_rx).await? {
                RecvNext::Frame(f) => f,
                RecvNext::Lagged(n) => {
                    warn!(
                        "[WebRTC] Play lagged {} frames stream='{}' — resync to IDR",
                        n, stream_id
                    );
                    streaming = false;
                    pace_next = Instant::now();
                    wait_start = Instant::now();
                    last_sent_ts = None;
                    request_publisher_keyframe(&stream_id);
                    pending.clear();
                    if let Some(idr) = drain_to_latest_idr(&mut rx) {
                        pending.push_back(idr);
                    }
                    continue;
                }
                RecvNext::Stopped => break,
            }
        };

        if frame.codec != CodecType::H264 && frame.codec != CodecType::H265 {
            continue;
        }

        received += 1;
        let mut sample_data = ensure_annex_b(&frame.data);
        if sample_data.is_empty() {
            continue;
        }

        store_live_nalu_config(&manager, &stream_id, &sample_data);

        if !config_ready {
            config_ready = stream_has_config(&manager, &stream_id);
        }

        if looks_like_h265_misread_as_h264(&sample_data) {
            warn!(
                "[WebRTC] Play stream='{}' received H265-like NAL (0x30)",
                stream_id
            );
        }

        let is_idr = frame.is_keyframe || contains_idr_nalu(&sample_data);
        let nalu_desc = describe_annex_b(&sample_data);

        if is_parameter_set_only(&sample_data) {
            skipped += 1;
            continue;
        }

        if streaming {
            if let Some(prev) = last_sent_ts {
                if is_rtp_timestamp_before(frame.timestamp, prev) && !is_idr {
                    debug!(
                        "[WebRTC] Play drop stale frame stream='{}' ts={} last={}",
                        stream_id, frame.timestamp, prev
                    );
                    skipped += 1;
                    continue;
                }
            }
        }

        if !streaming {
            let can_start = is_idr || frame.is_keyframe;
            if !can_start {
                if rtp_sent > 0 {
                    skipped += 1;
                    if skipped == 1 || skipped % 50 == 0 {
                        debug!(
                            "[WebRTC] Play wait IDR (resync) stream='{}' ts={} skipped={}",
                            stream_id, frame.timestamp, skipped
                        );
                    }
                    continue;
                }
                if wait_start.elapsed() < KEYFRAME_WAIT {
                    skipped += 1;
                    if skipped == 1 || skipped % 25 == 0 {
                        if skipped == 1 {
                            request_publisher_keyframe(&stream_id);
                        }
                        debug!(
                            "[WebRTC] Play wait IDR stream='{}' ts={} [{}] skipped={}",
                            stream_id, frame.timestamp, nalu_desc, skipped
                        );
                    }
                    continue;
                }
                warn!(
                    "[WebRTC] Play stream='{}' forcing start after {:?} (no IDR seen)",
                    stream_id, KEYFRAME_WAIT
                );
            }
            streaming = true;
            last_sent_ts = None;
            sample_data = prepend_stream_config(&manager, &stream_id, &sample_data);
            info!(
                "[WebRTC] Play streaming started stream='{}' ts={} [{}] idr={} replay={}",
                stream_id, frame.timestamp, nalu_desc, is_idr, is_replay
            );
        } else if is_idr || frame.is_keyframe {
            sample_data = prepend_stream_config(&manager, &stream_id, &sample_data);
        }

        let duration = PLAY_FRAME_DURATION;
        let now = Instant::now();
        if pace_next > now {
            tokio::time::sleep(pace_next - now).await;
        }
        outbound.send_access_unit(&sample_data, duration).await?;
        last_sent_ts = Some(frame.timestamp);
        pace_next = Instant::now() + duration;
        rtp_sent += 1;

        if rtp_sent <= 8 || is_idr || frame.is_keyframe {
            info!(
                "[WebRTC] Play sent stream='{}' #{} ts={} [{}] kf={}",
                stream_id,
                rtp_sent,
                frame.timestamp,
                nalu_desc,
                is_idr || frame.is_keyframe
            );
        } else if rtp_sent % 100 == 0 {
            info!(
                "[WebRTC] Play ~{} samples stream='{}' received={}",
                rtp_sent, stream_id, received
            );
        }
    }

    if rtp_sent == 0 {
        warn!(
            "[WebRTC] Play stream='{}' ended with ZERO samples (received={} skipped={})",
            stream_id, received, skipped
        );
    } else {
        warn!(
            "[WebRTC] Play relay ended stream='{}' samples={} received={} skipped={}",
            stream_id, rtp_sent, received, skipped
        );
    }
    Ok(())
}

fn prepend_stream_config(
    manager: &StreamManager,
    stream_id: &str,
    access_unit: &[u8],
) -> Vec<u8> {
    if contains_sps_or_pps_nalu(access_unit) {
        return access_unit.to_vec();
    }
    if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
        if let (Some(sps), Some(pps)) = (&stream.sps, &stream.pps) {
            return annex_b_with_config(sps, pps, access_unit);
        }
    }
    access_unit.to_vec()
}

fn is_idr_frame(frame: &MediaFrame) -> bool {
    if frame.is_keyframe {
        return true;
    }
    let data = ensure_annex_b(&frame.data);
    contains_idr_nalu(&data)
}

fn stop_requested(stop_rx: &mut tokio::sync::watch::Receiver<bool>) -> bool {
    stop_rx.has_changed().ok() == Some(true) && *stop_rx.borrow()
}

fn pc_connection_ended(pc: &RTCPeerConnection) -> bool {
    matches!(
        pc.connection_state(),
        RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed
    )
}

enum RecvNext {
    Frame(MediaFrame),
    Lagged(u64),
    Stopped,
}

async fn recv_next_frame(
    rx: &mut tokio::sync::broadcast::Receiver<MediaFrame>,
    stop_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<RecvNext> {
    use tokio::sync::broadcast::error::RecvError;

    loop {
        if stop_requested(stop_rx) {
            return Ok(RecvNext::Stopped);
        }
        tokio::select! {
            biased;
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    return Ok(RecvNext::Stopped);
                }
            }
            result = rx.recv() => {
                match result {
                    Ok(frame) => return Ok(RecvNext::Frame(frame)),
                    Err(RecvError::Lagged(n)) => {
                        if n > 0 {
                            return Ok(RecvNext::Lagged(n));
                        }
                    }
                    Err(RecvError::Closed) => return Ok(RecvNext::Stopped),
                }
            }
        }
    }
}

fn store_live_nalu_config(manager: &StreamManager, stream_id: &str, data: &[u8]) {
    let (sps, pps) = extract_sps_pps(data);
    if let (Some(sps), Some(pps)) = (sps, pps) {
        manager.set_stream_sps_pps(stream_id, sps, pps);
        return;
    }
    // Only fill missing SPS/PPS — do not overwrite SDP/sequence-header config from stray RTP NALUs.
    if stream_has_config(manager, stream_id) {
        return;
    }
    for (start, end) in iter_annex_b_nal_ranges(data) {
        manager.merge_stream_nalu_config(stream_id, &data[start..end]);
    }
}

