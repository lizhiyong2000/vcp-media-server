use anyhow::{anyhow, Result};
use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use webrtc::api::media_engine::{MIME_TYPE_H264, MIME_TYPE_HEVC};
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp::header::Header as RtpHeader;
use webrtc::rtp::packet::Packet as RtpPacket;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocalWriter;

use super::h264_util::{
    contains_idr_nalu, contains_sps_or_pps_nalu, describe_annex_b, duration_from_rtp_timestamps,
    ensure_annex_b, extract_sps_pps, is_rtp_timeline_reset, iter_annex_b_nal_ranges,
    looks_like_h265_misread_as_h264,
};
use super::outbound_h264::{annex_b_with_config, OutboundH264Track};
use super::peer::{new_peer_connection, wire_pc_debug, MIME_TYPE_H265};
use super::play_relay::{attach_relay_abort_handle, register_play_relay, unregister_play_relay};
use super::publish_signaling::request_publisher_keyframe;
use super::publisher::wire_ice_candidates;
use super::sdp_h264::{build_h264_sdp_fmtp, patch_answer_sdp_h264};
use super::signaling::ServerSignal;
use crate::core::dispatch::DispatchError;
use crate::core::{
    extract_hevc_param_sets, is_parameter_set_only as is_parameter_set_only_for_codec,
    packetize_hevc_access_unit_for_rtp, prepend_annex_b_config, CodecType, DispatchPolicy,
    MediaFrame, MetricsRegistry, PlaybackProtocol, StreamManager,
};
use webrtc::api::API;

pub use super::play_relay::{cancel_play_relay, signal_play_relay_stop};

pub struct PlaySession {
    pub answer_sdp: String,
    pub pc: Arc<RTCPeerConnection>,
    pub relay_id: String,
    pub relay_handle: tokio::task::JoinHandle<()>,
}

pub async fn start_play(
    api: Arc<API>,
    manager: Arc<StreamManager>,
    stream_id: String,
    offer_sdp: String,
    ice_tx: mpsc::UnboundedSender<ServerSignal>,
) -> Result<PlaySession> {
    validate_playable_stream(&manager, &stream_id)?;
    let video_codec = playable_video_codec(&manager, &stream_id)?;
    ensure_offer_supports_video_codec(&offer_sdp, video_codec)?;

    manager.ensure_stream_hub(&stream_id);
    log_stream_codec_state(&manager, &stream_id, "play-request");

    let h264_fmtp = if video_codec == CodecType::H264 {
        manager
            .get_stream(&stream_id.to_string())
            .and_then(|stream| match (&stream.sps, &stream.pps) {
                (Some(sps), Some(pps)) => Some(build_h264_sdp_fmtp(sps, pps)),
                _ => None,
            })
    } else {
        None
    };

    let pc = new_peer_connection(&api).await?;
    wire_pc_debug(pc.clone(), "play");

    let mut codec_capability = RTCRtpCodecCapability {
        mime_type: webrtc_mime_type(video_codec, &offer_sdp).to_owned(),
        clock_rate: 90000,
        ..Default::default()
    };
    if let Some(fmtp) = &h264_fmtp {
        codec_capability.sdp_fmtp_line = fmtp.clone();
        info!(
            "[WebRTC] Play track codec fmtp stream='{}' {}",
            stream_id, fmtp
        );
    }

    let outbound = if video_codec == CodecType::H265 {
        let track = Arc::new(TrackLocalStaticRTP::new(
            codec_capability,
            "video".to_owned(),
            stream_id.clone(),
        ));
        let _rtp_sender = pc
            .add_track(
                Arc::clone(&track) as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>
            )
            .await?;
        OutboundVideoTrack::H265 {
            track,
            seq: 1,
            ssrc: 0x2233_4455,
        }
    } else {
        let video_track = Arc::new(TrackLocalStaticSample::new(
            codec_capability,
            "video".to_owned(),
            stream_id.clone(),
        ));
        let _rtp_sender = pc
            .add_track(Arc::clone(&video_track)
                as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
            .await?;
        OutboundVideoTrack::H264(OutboundH264Track::new(video_track))
    };
    info!(
        "[WebRTC] Play added outbound RTP video track stream='{}' codec={:?}",
        stream_id, video_codec
    );

    wire_ice_candidates(pc.clone(), ice_tx.clone());

    let offer = RTCSessionDescription::offer(offer_sdp)?;
    pc.set_remote_description(offer).await?;
    info!("[WebRTC] Play set remote offer stream='{}'", stream_id);

    let answer = pc.create_answer(None).await?;
    pc.set_local_description(answer.clone()).await?;

    let mut answer_sdp = answer.sdp.clone();
    if video_codec == CodecType::H264 {
        if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
            if let (Some(sps), Some(pps)) = (&stream.sps, &stream.pps) {
                answer_sdp = patch_answer_sdp_h264(&answer_sdp, sps, pps);
            }
        }
    }

    info!(
        "[WebRTC] Play local answer ready stream='{}' sdp_len={}",
        stream_id,
        answer_sdp.len()
    );

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
            video_codec,
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
        relay_handle,
    })
}

fn validate_playable_stream(manager: &StreamManager, stream_id: &str) -> Result<()> {
    let stream = manager
        .get_stream(&stream_id.to_string())
        .ok_or_else(|| anyhow!("Stream '{}' not found", stream_id))?;

    if !stream.status.is_publishing() {
        return Err(anyhow!(
            "Stream '{}' is not publishing (status={})",
            stream_id,
            stream.status.as_str()
        ));
    }

    Ok(())
}

fn playable_video_codec(manager: &StreamManager, stream_id: &str) -> Result<CodecType> {
    let stream = manager
        .get_stream(&stream_id.to_string())
        .ok_or_else(|| anyhow!("Stream '{}' not found", stream_id))?;
    stream
        .tracks
        .iter()
        .find(|track| matches!(track.codec, CodecType::H264 | CodecType::H265))
        .map(|track| track.codec)
        .ok_or_else(|| anyhow!("Stream '{}' has no playable video track", stream_id))
}

fn webrtc_mime_type(codec: CodecType, offer_sdp: &str) -> &'static str {
    match codec {
        CodecType::H265 if offer_has_rtpmap_codec(offer_sdp, "H265") => MIME_TYPE_H265,
        CodecType::H265 => MIME_TYPE_HEVC,
        _ => MIME_TYPE_H264,
    }
}

fn ensure_offer_supports_video_codec(offer_sdp: &str, codec: CodecType) -> Result<()> {
    let supported = match codec {
        CodecType::H265 => {
            offer_has_rtpmap_codec(offer_sdp, "H265") || offer_has_rtpmap_codec(offer_sdp, "HEVC")
        }
        CodecType::H264 => offer_has_rtpmap_codec(offer_sdp, "H264"),
        _ => return Err(anyhow!("Unsupported WebRTC video codec {:?}", codec)),
    };
    if supported {
        Ok(())
    } else {
        Err(anyhow!(
            "WebRTC offer does not support required video codec {:?} for this stream",
            codec
        ))
    }
}

fn offer_has_rtpmap_codec(offer_sdp: &str, codec_name: &str) -> bool {
    let expected = format!("{}/90000", codec_name).to_ascii_lowercase();
    offer_sdp.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("a=rtpmap:") && lower.contains(&expected)
    })
}

fn log_stream_codec_state(manager: &StreamManager, stream_id: &str, phase: &str) {
    if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
        info!(
            "[WebRTC] Stream state [{}] id='{}' status={:?} tracks={:?} vps={} sps={} pps={}",
            phase,
            stream_id,
            stream.status,
            stream.tracks,
            stream.vps.as_ref().map(|v| v.len()).unwrap_or(0),
            stream.sps.as_ref().map(|s| s.len()).unwrap_or(0),
            stream.pps.as_ref().map(|p| p.len()).unwrap_or(0)
        );
    } else {
        warn!(
            "[WebRTC] Stream state [{}] id='{}' NOT FOUND",
            phase, stream_id
        );
    }
}

async fn wait_for_connected(
    pc: &Arc<RTCPeerConnection>,
    manager: &StreamManager,
    stream_id: &str,
    label: &str,
) -> Result<()> {
    for attempt in 0..100 {
        match pc.connection_state() {
            RTCPeerConnectionState::Connected => {
                log_stream_codec_state(manager, stream_id, "connected");
                info!(
                    "[WebRTC] {} peer connection connected (attempt={})",
                    label, attempt
                );
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

fn stream_has_config(manager: &StreamManager, stream_id: &str, codec: CodecType) -> bool {
    manager
        .get_stream(&stream_id.to_string())
        .map(|s| match codec {
            CodecType::H265 => s.vps.is_some() && s.sps.is_some() && s.pps.is_some(),
            CodecType::H264 => s.sps.is_some() && s.pps.is_some(),
            _ => false,
        })
        .unwrap_or(false)
}

enum OutboundVideoTrack {
    H264(OutboundH264Track),
    H265 {
        track: Arc<TrackLocalStaticRTP>,
        seq: u16,
        ssrc: u32,
    },
}

impl OutboundVideoTrack {
    async fn wait_binding(&self, label: &str) -> Result<()> {
        match self {
            OutboundVideoTrack::H264(track) => track.wait_binding(label).await,
            // TrackLocalStaticRTP has no public binding count; relay starts only after
            // PeerConnection is connected, so the RTP binding should already be installed.
            OutboundVideoTrack::H265 { .. } => Ok(()),
        }
    }

    async fn send_access_unit(
        &mut self,
        codec: CodecType,
        access_unit: &[u8],
        duration: Duration,
        timestamp: u64,
    ) -> Result<()> {
        match self {
            OutboundVideoTrack::H264(track) => track.send_access_unit(access_unit, duration).await,
            OutboundVideoTrack::H265 { track, seq, ssrc } => {
                let rtp_ts = (timestamp & 0xFFFF_FFFF) as u32;
                let packets =
                    packetize_hevc_access_unit_for_rtp(access_unit, 96, seq, rtp_ts, *ssrc);
                for packet in &packets {
                    let rtp = parse_generated_rtp_packet(packet)?;
                    track.write_rtp(&rtp).await?;
                }
                if packets.is_empty() && codec == CodecType::H265 {
                    debug!("[WebRTC] H265 access unit generated zero RTP packets");
                }
                Ok(())
            }
        }
    }
}

fn parse_generated_rtp_packet(packet: &[u8]) -> Result<RtpPacket> {
    if packet.len() < 12 {
        return Err(anyhow!(
            "generated RTP packet too short: {} bytes",
            packet.len()
        ));
    }
    Ok(RtpPacket {
        header: RtpHeader {
            version: packet[0] >> 6,
            padding: false,
            extension: false,
            marker: (packet[1] & 0x80) != 0,
            payload_type: packet[1] & 0x7F,
            sequence_number: u16::from_be_bytes([packet[2], packet[3]]),
            timestamp: u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]),
            ssrc: u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]),
            csrc: Vec::new(),
            extension_profile: 0,
            extensions: Vec::new(),
            extensions_padding: 0,
        },
        payload: Bytes::copy_from_slice(&packet[12..]),
    })
}

async fn relay_stream_to_track(
    manager: Arc<StreamManager>,
    stream_id: String,
    mut outbound: OutboundVideoTrack,
    pc: Arc<RTCPeerConnection>,
    mut stop_rx: tokio::sync::watch::Receiver<bool>,
    is_late_joiner: bool,
    video_codec: CodecType,
    relay_id: &str,
) -> Result<()> {
    let _guard = RelayCleanup(relay_id.to_string());

    struct RelayCleanup(String);
    impl Drop for RelayCleanup {
        fn drop(&mut self) {
            unregister_play_relay(&self.0);
        }
    }

    let Some(mut reader) = manager.dispatch_subscribe(&stream_id, DispatchPolicy::WebRtcPlay)
    else {
        return Err(anyhow!("No StreamHub for stream {}", stream_id));
    };

    wait_for_connected(&pc, &manager, &stream_id, "play").await?;
    if stop_requested(&mut stop_rx) {
        return Ok(());
    }

    outbound.wait_binding("play").await?;
    log_stream_codec_state(&manager, &stream_id, "relay-start");

    let mut config_ready = stream_has_config(&manager, &stream_id, video_codec);
    if !config_ready {
        warn!(
            "[WebRTC] Play stream='{}' no video config yet codec={:?}, wait for live config late_joiner={}",
            stream_id, video_codec, is_late_joiner
        );
    }

    let mut pending: VecDeque<MediaFrame> = VecDeque::new();
    if let Some(idr) = reader.prime_from_idr(&manager, &stream_id).await {
        pending.push_back(idr);
    } else {
        warn!(
            "[WebRTC] Play stream='{}' no IDR primed — waiting for live keyframe",
            stream_id
        );
    }

    let mut rtp_sent: u64 = 0;
    let mut received: u64 = 0;
    let mut skipped: u64 = 0;
    let mut streaming = false;
    let mut last_sent_ts: Option<u64> = None;
    let mut wait_start = Instant::now();
    let mut last_keyframe_request = Instant::now();
    const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_secs(1);

    info!(
        "[WebRTC] Play relay loop started for stream='{}'",
        stream_id
    );

    loop {
        if stop_requested(&stop_rx) {
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
            match recv_coalesced_dispatch(&mut reader, &mut stop_rx).await? {
                RecvCoalesced::Frame(f, n) => (f, n),
                RecvCoalesced::Lagged(n) => {
                    warn!(
                        "[WebRTC] Play lagged {} frames stream='{}' — jump to live",
                        n, stream_id
                    );
                    streaming = false;
                    wait_start = Instant::now();
                    last_sent_ts = None;
                    request_publisher_keyframe(&stream_id);
                    pending.clear();
                    reader.recover_lag(&stream_id, n);
                    reader.snap_to_latest_idr();
                    if let Some(idr) = reader.prime_from_idr(&manager, &stream_id).await {
                        pending.push_back(idr);
                    }
                    continue;
                }
                RecvCoalesced::Stopped => break,
            }
        };

        if frame.codec != video_codec {
            continue;
        }

        received += 1;
        let mut sample_data = ensure_annex_b(&frame.data);
        if sample_data.is_empty() {
            continue;
        }

        store_live_nalu_config(&manager, &stream_id, video_codec, &sample_data);

        if !config_ready {
            config_ready = stream_has_config(&manager, &stream_id, video_codec);
        }

        if video_codec == CodecType::H264 && looks_like_h265_misread_as_h264(&sample_data) {
            warn!(
                "[WebRTC] Play stream='{}' received H265-like NAL (0x30)",
                stream_id
            );
        }

        let is_idr = frame.is_keyframe || contains_idr_nalu(&sample_data);
        let nalu_desc = describe_annex_b(&sample_data);

        if is_parameter_set_only_for_codec(video_codec, &sample_data) {
            skipped += 1;
            continue;
        }

        if streaming {
            if let Some(prev) = last_sent_ts {
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
                        reader.snap_to_latest_idr();
                        if let Some(idr) = reader.hub().latest_idr_frame() {
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
            sample_data = prepend_stream_config(&manager, &stream_id, video_codec, &sample_data);
            info!(
                "[WebRTC] Play streaming started stream='{}' relay='{}' codec={:?} ts={} [{}] idr={} late_joiner={}",
                stream_id, relay_id, video_codec, frame.timestamp, nalu_desc, is_idr, is_late_joiner
            );
        } else if is_idr || frame.is_keyframe {
            sample_data = prepend_stream_config(&manager, &stream_id, video_codec, &sample_data);
        }

        let lag = reader.hub().latest_seq().saturating_sub(reader.cursor());
        let catch_up = lag > 2 || coalesced > 0;
        let duration = if catch_up {
            Duration::from_millis(1)
        } else {
            duration_from_rtp_timestamps(last_sent_ts, frame.timestamp)
        };
        if stop_requested(&stop_rx) {
            break;
        }
        tokio::select! {
            biased;
            _ = stop_rx.changed() => {
                if stop_requested(&stop_rx) {
                    info!("[WebRTC] Play relay stop during send stream='{}'", stream_id);
                    break;
                }
            }
            result = outbound.send_access_unit(video_codec, &sample_data, duration, frame.timestamp) => {
                result?;
            }
        }
        last_sent_ts = Some(frame.timestamp);
        rtp_sent += 1;
        if rtp_sent == 1 {
            MetricsRegistry::record_first_frame_ms(
                &stream_id,
                PlaybackProtocol::Webrtc,
                wait_start.elapsed().as_millis() as u64,
            );
        }

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
    codec: CodecType,
    access_unit: &[u8],
) -> Vec<u8> {
    if access_unit_has_codec_config(codec, access_unit) {
        return access_unit.to_vec();
    }
    if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
        match codec {
            CodecType::H265 => {
                return prepend_annex_b_config(
                    codec,
                    stream.vps.as_deref(),
                    stream.sps.as_deref(),
                    stream.pps.as_deref(),
                    access_unit,
                );
            }
            CodecType::H264 => {
                if let (Some(sps), Some(pps)) = (&stream.sps, &stream.pps) {
                    return annex_b_with_config(sps, pps, access_unit);
                }
            }
            _ => {}
        }
    }
    access_unit.to_vec()
}

fn access_unit_has_codec_config(codec: CodecType, access_unit: &[u8]) -> bool {
    match codec {
        CodecType::H265 => {
            let (vps, sps, pps) = extract_hevc_param_sets(access_unit);
            vps.is_some() || sps.is_some() || pps.is_some()
        }
        CodecType::H264 => contains_sps_or_pps_nalu(access_unit),
        _ => false,
    }
}

fn stop_requested(stop_rx: &tokio::sync::watch::Receiver<bool>) -> bool {
    *stop_rx.borrow()
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

/// Block for one frame via DispatchReader, coalescing video bursts.
async fn recv_coalesced_dispatch(
    reader: &mut crate::core::DispatchReader,
    stop_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<RecvCoalesced> {
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
            result = reader.recv_coalesced() => {
                match result {
                    Ok(frame) => return Ok(RecvCoalesced::Frame(frame, 0)),
                    Err(DispatchError::Closed) => return Ok(RecvCoalesced::Stopped),
                }
            }
        }
    }
}

fn store_live_nalu_config(manager: &StreamManager, stream_id: &str, codec: CodecType, data: &[u8]) {
    if codec == CodecType::H265 {
        let (vps, sps, pps) = extract_hevc_param_sets(data);
        if let (Some(vps), Some(sps), Some(pps)) = (vps, sps, pps) {
            manager.set_stream_hevc_params(stream_id, vps, sps, pps);
        }
        return;
    }

    let (sps, pps) = extract_sps_pps(data);
    if let (Some(sps), Some(pps)) = (sps, pps) {
        manager.set_stream_sps_pps(stream_id, sps, pps);
        return;
    }
    // Only fill missing SPS/PPS — do not overwrite SDP/sequence-header config from stray RTP NALUs.
    if stream_has_config(manager, stream_id, codec) {
        return;
    }
    for (start, end) in iter_annex_b_nal_ranges(data) {
        manager.merge_stream_nalu_config(stream_id, codec, &data[start..end]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{StreamProtocol, StreamSourceMode};

    #[test]
    fn validate_playable_stream_rejects_missing_stream() {
        let manager = StreamManager::new();

        let err = validate_playable_stream(&manager, "missing").unwrap_err();

        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn validate_playable_stream_rejects_non_publishing_stream() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::WebRTC, None);

        let err = validate_playable_stream(&manager, "s").unwrap_err();
        assert!(err.to_string().contains("not publishing"));

        manager.set_unpublished("s").unwrap();
        let err = validate_playable_stream(&manager, "s").unwrap_err();
        assert!(err.to_string().contains("status=unpublished"));
    }

    #[test]
    fn validate_playable_stream_accepts_publishing_stream() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::WebRTC, None);
        manager.set_publishing("s").unwrap();

        validate_playable_stream(&manager, "s").unwrap();
    }

    #[test]
    fn playable_video_codec_detects_h265_track() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::RTMP, None);
        manager.set_stream_hevc_params(
            "s",
            vec![0x40, 0x01, 0x0c, 0x01],
            vec![0x42, 0x01, 0x01, 0x60],
            vec![0x44, 0x01, 0xc0, 0x73],
        );

        assert_eq!(
            playable_video_codec(&manager, "s").unwrap(),
            CodecType::H265
        );
    }

    #[test]
    fn h265_play_requires_hevc_offer() {
        let h264_offer = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 H264/90000\r\n";
        let hevc_offer = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 126\r\na=rtpmap:126 HEVC/90000\r\n";
        let h265_offer = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 127\r\na=rtpmap:127 H265/90000\r\n";

        assert!(ensure_offer_supports_video_codec(h264_offer, CodecType::H265).is_err());
        assert!(ensure_offer_supports_video_codec(hevc_offer, CodecType::H265).is_ok());
        assert!(ensure_offer_supports_video_codec(h265_offer, CodecType::H265).is_ok());
        assert_eq!(
            webrtc_mime_type(CodecType::H265, h265_offer),
            MIME_TYPE_H265
        );
        assert!(ensure_offer_supports_video_codec(h264_offer, CodecType::H264).is_ok());
    }

    #[test]
    fn h265_prepend_stream_config_includes_vps_sps_pps() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Push, StreamProtocol::RTMP, None);
        let vps = vec![0x40, 0x01, 0x0c, 0x01];
        let sps = vec![0x42, 0x01, 0x01, 0x60];
        let pps = vec![0x44, 0x01, 0xc0, 0x73];
        manager.set_stream_hevc_params("s", vps.clone(), sps.clone(), pps.clone());
        let cra = [0x00, 0x00, 0x00, 0x01, 0x2a, 0x01, 0xaf];

        let data = prepend_stream_config(&manager, "s", CodecType::H265, &cra);

        assert!(data.windows(vps.len()).any(|w| w == vps.as_slice()));
        assert!(data.windows(sps.len()).any(|w| w == sps.as_slice()));
        assert!(data.windows(pps.len()).any(|w| w == pps.as_slice()));
        assert!(data.ends_with(&cra));
    }
}
