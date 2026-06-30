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
    contains_idr_nalu, contains_sps_or_pps_nalu, describe_annex_b, duration_from_rtp_timestamps,
    ensure_annex_b, extract_sps_pps, is_parameter_set_only, is_rtp_stale_in_gop,
    is_rtp_timestamp_before, is_rtp_timeline_reset,
    iter_annex_b_nal_ranges, looks_like_h265_misread_as_h264,
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

pub struct PlaySession {
    pub answer_sdp: String,
    pub pc: Arc<RTCPeerConnection>,
    pub relay_id: String,
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
    let (relay_id, stop_rx, active_players) = register_play_relay(&stream_id);
    let manager_clone = manager.clone();
    let sid = stream_id.clone();
    let rid = relay_id.clone();
    let pc_clone = pc.clone();
    let relay_handle = tokio::spawn(async move {
        let result = relay_stream_to_track(
            manager_clone,
            sid.clone(),
            outbound,
            pc_clone,
            stop_rx,
            active_players > 1,
            &rid,
        )
        .await;
        if let Err(e) = result {
            error!(
                "[WebRTC] Play relay error stream='{}' relay='{}': {}",
                sid, rid, e
            );
        }
    });
    attach_relay_abort_handle(&relay_id, relay_handle.abort_handle());

    info!(
        "[WebRTC] Play session ready stream='{}' relay='{}' active_players={}",
        stream_id, relay_id, active_players
    );

    Ok(PlaySession {
        answer_sdp,
        pc,
        relay_id,
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

async fn wait_for_connected(
    pc: &Arc<RTCPeerConnection>,
    rx: &mut tokio::sync::broadcast::Receiver<MediaFrame>,
    manager: &StreamManager,
    stream_id: &str,
    label: &str,
) -> Result<()> {
    for attempt in 0..100 {
        drain_config_from_rx(rx, manager, stream_id);

        match pc.connection_state() {
            RTCPeerConnectionState::Connected => {
                drain_config_from_rx(rx, manager, stream_id);
                info!("[WebRTC] {} peer connection connected (attempt={})", label, attempt);
                return Ok(());
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
                        "[WebRTC] {} waiting connected: {:?} (attempt={})",
                        label,
                        pc.connection_state(),
                        attempt
                    );
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    Err(anyhow!("{} timeout waiting for peer connection", label))
}

fn drain_config_from_rx(
    rx: &mut tokio::sync::broadcast::Receiver<MediaFrame>,
    manager: &StreamManager,
    stream_id: &str,
) {
    use tokio::sync::broadcast::error::TryRecvError;

    loop {
        match rx.try_recv() {
            Ok(frame) => {
                if frame.codec == CodecType::H264 || frame.codec == CodecType::H265 {
                    store_live_nalu_config(manager, stream_id, &ensure_annex_b(&frame.data));
                }
            }
            Err(TryRecvError::Lagged(_)) => continue,
            Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
        }
    }
}

/// Drop all queued frames (keep SPS/PPS). Returns number of frames discarded.
fn flush_stale_rx(
    rx: &mut tokio::sync::broadcast::Receiver<MediaFrame>,
    manager: &StreamManager,
    stream_id: &str,
) -> Option<u64> {
    use tokio::sync::broadcast::error::TryRecvError;

    let mut dropped = 0u64;
    loop {
        match rx.try_recv() {
            Ok(frame) => {
                dropped += 1;
                if frame.codec == CodecType::H264 || frame.codec == CodecType::H265 {
                    store_live_nalu_config(manager, stream_id, &ensure_annex_b(&frame.data));
                }
            }
            Err(TryRecvError::Lagged(n)) => {
                dropped += n;
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
        }
    }
    if dropped > 0 { Some(dropped) } else { None }
}

fn is_playable_video_frame(frame: &MediaFrame) -> bool {
    if frame.codec != CodecType::H264 && frame.codec != CodecType::H265 {
        return false;
    }
    let data = ensure_annex_b(&frame.data);
    !data.is_empty() && !is_parameter_set_only(&data)
}

/// Drain broadcast buffer and return the most recently received IDR (not max timestamp).
/// After replaceTrack the newest IDR often has a lower RTP timestamp than older frames.
fn drain_to_most_recent_idr(
    rx: &mut tokio::sync::broadcast::Receiver<MediaFrame>,
) -> Option<MediaFrame> {
    use tokio::sync::broadcast::error::TryRecvError;

    let mut last_idr: Option<MediaFrame> = None;
    loop {
        match rx.try_recv() {
            Ok(frame) => {
                if is_idr_frame(&frame) {
                    last_idr = Some(frame);
                }
            }
            Err(TryRecvError::Lagged(_)) => continue,
            Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
        }
    }
    last_idr
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
    is_late_joiner: bool,
    relay_id: &str,
) -> Result<()> {
    let _guard = RelayCleanup(relay_id.to_string());

    struct RelayCleanup(String);
    impl Drop for RelayCleanup {
        fn drop(&mut self) {
            unregister_play_relay(&self.0);
        }
    }

    let Some(mut rx) = manager.subscribe(&stream_id) else {
        return Err(anyhow!("No broadcast channel for stream {}", stream_id));
    };

    wait_for_connected(&pc, &mut rx, &manager, &stream_id, "play").await?;
    if stop_requested(&mut stop_rx) {
        return Ok(());
    }

    outbound.wait_binding("play").await?;
    log_stream_codec_state(&manager, &stream_id, "relay-start");

    let mut config_ready = stream_has_config(&manager, &stream_id);
    if !config_ready {
        warn!(
            "[WebRTC] Play stream='{}' no SPS/PPS yet, wait for live config late_joiner={}",
            stream_id, is_late_joiner
        );
    }

    let mut pending: VecDeque<MediaFrame> = VecDeque::new();
    if let Some(idr) = prime_play_idr(&mut rx, &stream_id).await {
        pending.push_back(idr);
    } else {
        warn!(
            "[WebRTC] Play stream='{}' no IDR primed — waiting for live keyframe",
            stream_id
        );
    }
    // Drop any frames buffered during ICE / prime; start from live edge.
    if let Some(skipped) = flush_stale_rx(&mut rx, &manager, &stream_id) {
        info!(
            "[WebRTC] Play stream='{}' flushed {} stale frames before live edge",
            stream_id, skipped
        );
    }

    let mut rtp_sent: u64 = 0;
    let mut received: u64 = 0;
    let mut skipped: u64 = 0;
    let mut streaming = false;
    let mut pace_next = Instant::now();
    let mut last_sent_ts: Option<u64> = None;
    let mut wait_start = Instant::now();
    let mut last_keyframe_request = Instant::now();
    const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_secs(1);

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

        let (frame, coalesced) = if let Some(f) = pending.pop_front() {
            (f, 0u64)
        } else {
            match recv_coalesced_frame(&mut rx, &mut stop_rx).await? {
                RecvCoalesced::Frame(f, n) => (f, n),
                RecvCoalesced::Lagged(n) => {
                    warn!(
                        "[WebRTC] Play lagged {} frames stream='{}' — jump to live",
                        n, stream_id
                    );
                    streaming = false;
                    pace_next = Instant::now();
                    wait_start = Instant::now();
                    last_sent_ts = None;
                    request_publisher_keyframe(&stream_id);
                    pending.clear();
                    let _ = flush_stale_rx(&mut rx, &manager, &stream_id);
                    if let Some(idr) = drain_to_most_recent_idr(&mut rx) {
                        pending.push_back(idr);
                    }
                    continue;
                }
                RecvCoalesced::Stopped => break,
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
                if is_rtp_stale_in_gop(frame.timestamp, prev) {
                    skipped += 1;
                    continue;
                }
                if is_rtp_timeline_reset(frame.timestamp, prev) {
                    if is_idr || frame.is_keyframe {
                        info!(
                            "[WebRTC] Play new timeline IDR stream='{}' {} -> {}",
                            stream_id, prev, frame.timestamp
                        );
                        last_sent_ts = None;
                    } else {
                        debug!(
                            "[WebRTC] Play new timeline without IDR stream='{}' {} -> {}",
                            stream_id, prev, frame.timestamp
                        );
                        streaming = false;
                        last_sent_ts = None;
                        request_publisher_keyframe(&stream_id);
                        last_keyframe_request = Instant::now();
                        skipped += 1;
                        if let Some(idr) = drain_to_most_recent_idr(&mut rx) {
                            pending.push_back(idr);
                        }
                        continue;
                    }
                }
            }
        }

        if !streaming {
            let can_start = is_idr || frame.is_keyframe;
            if !can_start {
                skipped += 1;
                if last_keyframe_request.elapsed() >= KEYFRAME_REQUEST_INTERVAL {
                    request_publisher_keyframe(&stream_id);
                    last_keyframe_request = Instant::now();
                }
                if skipped == 1 || skipped % 25 == 0 {
                    debug!(
                        "[WebRTC] Play wait IDR stream='{}' ts={} [{}] skipped={} elapsed={:?}",
                        stream_id,
                        frame.timestamp,
                        nalu_desc,
                        skipped,
                        wait_start.elapsed()
                    );
                }
                continue;
            }
            streaming = true;
            last_sent_ts = None;
            sample_data = prepend_stream_config(&manager, &stream_id, &sample_data);
            info!(
                "[WebRTC] Play streaming started stream='{}' relay='{}' ts={} [{}] idr={} late_joiner={}",
                stream_id, relay_id, frame.timestamp, nalu_desc, is_idr, is_late_joiner
            );
        } else if is_idr || frame.is_keyframe {
            sample_data = prepend_stream_config(&manager, &stream_id, &sample_data);
        }

        let duration = duration_from_rtp_timestamps(last_sent_ts, frame.timestamp);
        let catch_up = coalesced > 0;
        if !catch_up {
            let now = Instant::now();
            if pace_next > now {
                tokio::time::sleep(pace_next - now).await;
            }
            pace_next = Instant::now() + duration;
        } else {
            pace_next = Instant::now();
        }
        outbound.send_access_unit(&sample_data, duration).await?;
        last_sent_ts = Some(frame.timestamp);
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

/// Wait for publisher keyframe, then pick the most recently received live IDR.
async fn prime_play_idr(
    rx: &mut tokio::sync::broadcast::Receiver<MediaFrame>,
    stream_id: &str,
) -> Option<MediaFrame> {
    request_publisher_keyframe(stream_id);
    let deadline = Instant::now() + Duration::from_millis(800);
    let mut last_idr: Option<MediaFrame> = None;

    while Instant::now() < deadline {
        if let Some(idr) = drain_to_most_recent_idr(rx) {
            last_idr = Some(idr);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    if let Some(ref idr) = last_idr {
        info!(
            "[WebRTC] Play primed live IDR stream='{}' ts={}",
            stream_id, idr.timestamp
        );
    }
    last_idr
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

enum RecvCoalesced {
    Frame(MediaFrame, u64),
    Lagged(u64),
    Stopped,
}

/// Block for one frame, then coalesce any burst to the latest playable frame.
async fn recv_coalesced_frame(
    rx: &mut tokio::sync::broadcast::Receiver<MediaFrame>,
    stop_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<RecvCoalesced> {
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};

    loop {
        if stop_requested(stop_rx) {
            return Ok(RecvCoalesced::Stopped);
        }
        tokio::select! {
            biased;
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    return Ok(RecvCoalesced::Stopped);
                }
            }
            result = rx.recv() => {
                match result {
                    Ok(mut latest) => {
                        let mut skipped = 0u64;
                        loop {
                            match rx.try_recv() {
                                Ok(next) => {
                                    if is_playable_video_frame(&next) {
                                        skipped += 1;
                                        latest = next;
                                    }
                                }
                                Err(TryRecvError::Lagged(n)) => {
                                    return Ok(RecvCoalesced::Lagged(n));
                                }
                                Err(TryRecvError::Closed) => return Ok(RecvCoalesced::Stopped),
                                Err(TryRecvError::Empty) => break,
                            }
                        }
                        return Ok(RecvCoalesced::Frame(latest, skipped));
                    }
                    Err(RecvError::Lagged(n)) => {
                        if n > 0 {
                            return Ok(RecvCoalesced::Lagged(n));
                        }
                    }
                    Err(RecvError::Closed) => return Ok(RecvCoalesced::Stopped),
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

