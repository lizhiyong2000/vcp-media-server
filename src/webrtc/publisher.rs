use anyhow::{anyhow, Result};
use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp::codecs::h264::H264Packet;
use webrtc::rtp::packet::Packet;
use webrtc::rtp::packetizer::Depacketizer;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
use webrtc::track::track_remote::TrackRemote;

use super::h264_util::{describe_annex_b, is_keyframe_annex_b, is_parameter_set_only};
use super::peer::{new_peer_connection, wire_pc_debug};
use super::rtp_h264::{
    self, annex_b_from_rtp_payload, describe_rtp_payload, extract_sps_pps_from_nalus, hex_prefix,
    is_idr_rtp_payload, parse_rtp_h264,
};
use super::sdp_h264::parse_sprop_parameter_sets;
use super::signaling::ServerSignal;
use crate::core::{
    default_live_tracks, CodecType, MediaFrame, StreamManager, StreamProtocol, StreamSourceMode,
};
use webrtc::api::API;

pub struct PublishSession {
    pub answer_sdp: String,
    pub pc: Arc<RTCPeerConnection>,
}

pub async fn start_publish(
    api: Arc<API>,
    manager: Arc<StreamManager>,
    stream_id: String,
    publisher_id: String,
    offer_sdp: String,
    ice_tx: mpsc::UnboundedSender<ServerSignal>,
) -> Result<PublishSession> {
    let pc = new_peer_connection(&api).await?;
    wire_pc_debug(pc.clone(), "publish");

    manager.create_stream(
        &stream_id,
        StreamSourceMode::Push,
        StreamProtocol::WebRTC,
        None,
    );
    manager.acquire_publisher(&stream_id, &publisher_id)?;
    manager.set_stream_tracks(&stream_id, default_live_tracks());
    let _ = manager.set_unpublished(&stream_id);
    manager.ensure_stream_broadcast(&stream_id);

    let (sdp_sps, sdp_pps) = parse_sprop_parameter_sets(&offer_sdp);
    if let (Some(sps), Some(pps)) = (sdp_sps, sdp_pps) {
        info!(
            "[WebRTC] Publish primed SPS/PPS from offer SDP stream='{}' sps={} pps={}",
            stream_id,
            sps.len(),
            pps.len()
        );
        manager.set_stream_sps_pps(&stream_id, sps, pps);
    }

    let manager_track = manager.clone();
    let sid = stream_id.clone();
    pc.on_track(Box::new(move |track, _receiver, transceiver| {
        info!(
            "[WebRTC] on_track stream='{}' kind={:?} id={} mid={:?}",
            sid,
            track.kind(),
            track.id(),
            transceiver.mid()
        );
        let manager_track = manager_track.clone();
        let sid = sid.clone();
        let sid_for_task = sid.clone();
        Box::pin(async move {
            if let Err(e) = read_track_to_stream(manager_track, sid_for_task.clone(), track).await {
                error!(
                    "[WebRTC] Publish track error stream='{}': {}",
                    sid_for_task, e
                );
            }
        })
    }));

    wire_ice_candidates(pc.clone(), ice_tx.clone());

    let offer = match RTCSessionDescription::offer(offer_sdp) {
        Ok(offer) => offer,
        Err(e) => {
            manager.release_publisher(&stream_id, &publisher_id);
            return Err(e.into());
        }
    };
    if let Err(e) = pc.set_remote_description(offer).await {
        manager.release_publisher(&stream_id, &publisher_id);
        return Err(e.into());
    }
    info!("[WebRTC] Publish set remote offer stream='{}'", stream_id);

    let answer = match pc.create_answer(None).await {
        Ok(answer) => answer,
        Err(e) => {
            manager.release_publisher(&stream_id, &publisher_id);
            return Err(e.into());
        }
    };
    if let Err(e) = pc.set_local_description(answer.clone()).await {
        manager.release_publisher(&stream_id, &publisher_id);
        return Err(e.into());
    }
    info!(
        "[WebRTC] Publish local answer ready stream='{}' sdp_len={}",
        stream_id,
        answer.sdp.len()
    );

    let _ = manager.set_publishing(&stream_id);
    info!("[WebRTC] Publish session ready for stream '{}'", stream_id);

    Ok(PublishSession {
        answer_sdp: answer.sdp,
        pc,
    })
}

async fn read_track_to_stream(
    manager: Arc<StreamManager>,
    stream_id: String,
    track: Arc<TrackRemote>,
) -> Result<()> {
    let kind = track.kind();
    let codec = track.codec();
    info!(
        "[WebRTC] Reading incoming track kind={:?} stream_id='{}' mime={} clock={}",
        kind, stream_id, codec.capability.mime_type, codec.capability.clock_rate
    );

    if kind == RTPCodecType::Video {
        let mime = codec.capability.mime_type.to_lowercase();
        if !mime.contains("h264") {
            warn!(
                "[WebRTC] Incoming video codec is '{}' (expected H264). \
                 WebRTC relay only supports H264; playback will fail. \
                 Force H264 in the browser via setCodecPreferences.",
                codec.capability.mime_type
            );
        }
        read_h264_track(manager, stream_id, track).await
    } else if kind == RTPCodecType::Audio {
        read_audio_track(manager, stream_id, track).await
    } else {
        warn!("[WebRTC] Unsupported track kind {:?}", kind);
        Ok(())
    }
}

async fn read_h264_track(
    manager: Arc<StreamManager>,
    stream_id: String,
    track: Arc<TrackRemote>,
) -> Result<()> {
    let mut depacketizer = H264Packet::default();
    let mut access_units: u64 = 0;
    let mut depacketize_fail: u64 = 0;
    let mut empty_nalu: u64 = 0;
    let mut batch = AccessUnitBatch::default();
    let mut last_pkt_ts: Option<u32> = None;

    while let Ok((pkt, _attrs)) = track.read_rtp().await {
        if access_units == 0 && batch.parts.is_empty() {
            info!(
                "[WebRTC] First RTP packet stream='{}' pt={} seq={} ts={} payload={}B hex={} [{}]",
                stream_id,
                pkt.header.payload_type,
                pkt.header.sequence_number,
                pkt.header.timestamp,
                pkt.payload.len(),
                hex_prefix(&pkt.payload, 16),
                describe_rtp_payload(&pkt.payload)
            );
        }

        let pkt_ts = pkt.header.timestamp;
        if let Some(prev_ts) = last_pkt_ts {
            let backward = prev_ts.wrapping_sub(pkt_ts);
            // replaceTrack / encoder restart often resets the RTP clock backward.
            if backward > 3000 && backward < 0x8000_0000 {
                info!(
                    "[WebRTC] RTP timestamp reset stream='{}' {} -> {}, flush depacketizer",
                    stream_id, prev_ts, pkt_ts
                );
                depacketizer = H264Packet::default();
                if !batch.parts.is_empty() {
                    access_units += publish_access_unit(&manager, &stream_id, &mut batch);
                }
            }
        }
        last_pkt_ts = Some(pkt_ts);

        let marker = pkt.header.marker;
        match h264_rtp_to_frame(&mut depacketizer, &pkt, &stream_id, &manager) {
            Some(frame) => {
                if is_parameter_set_only(&frame.data) {
                    continue;
                }
                if !batch.parts.is_empty() && batch.timestamp != pkt_ts {
                    access_units += publish_access_unit(&manager, &stream_id, &mut batch);
                }
                if batch.parts.is_empty() {
                    batch.timestamp = pkt_ts;
                }
                batch.parts.push(frame.data);
                batch.is_keyframe |= frame.is_keyframe;
                // Flush only on RTP marker — do not flush per IDR slice (same timestamp
                // multi-slice keyframes must be one access unit or playback corrupts).
                if marker {
                    access_units += publish_access_unit(&manager, &stream_id, &mut batch);
                }
            }
            None => {
                if pkt.payload.is_empty() {
                    empty_nalu += 1;
                } else {
                    depacketize_fail += 1;
                    if depacketize_fail <= 10 || depacketize_fail % 100 == 0 {
                        warn!(
                            "[WebRTC] depacketize skip stream='{}' fail={} pt={} payload={}B marker={} hex={} [{}]",
                            stream_id,
                            depacketize_fail,
                            pkt.header.payload_type,
                            pkt.payload.len(),
                            pkt.header.marker,
                            hex_prefix(&pkt.payload, 12),
                            describe_rtp_payload(&pkt.payload)
                        );
                    }
                }
            }
        }
    }

    if !batch.parts.is_empty() {
        access_units += publish_access_unit(&manager, &stream_id, &mut batch);
    }

    let _ = manager.set_unpublished(&stream_id);
    info!(
        "[WebRTC] Publish track ended stream='{}' access_units={} depacketize_fail={} empty={}",
        stream_id, access_units, depacketize_fail, empty_nalu
    );
    Ok(())
}

#[derive(Default)]
struct AccessUnitBatch {
    timestamp: u32,
    parts: Vec<Bytes>,
    is_keyframe: bool,
}

fn publish_access_unit(
    manager: &StreamManager,
    stream_id: &str,
    batch: &mut AccessUnitBatch,
) -> u64 {
    if batch.parts.is_empty() {
        return 0;
    }

    let mut combined = Vec::new();
    for part in &batch.parts {
        combined.extend_from_slice(part);
    }
    let is_keyframe = batch.is_keyframe || is_keyframe_annex_b(&combined);
    let desc = describe_annex_b(&combined);
    static UNITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = UNITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

    if n == 1 {
        info!(
            "[WebRTC] First published access unit stream='{}' size={} keyframe={} ts={} [{}]",
            stream_id,
            combined.len(),
            is_keyframe,
            batch.timestamp,
            desc
        );
    } else if n <= 5 || is_keyframe {
        info!(
            "[WebRTC] Published access unit #{} stream='{}' keyframe={} ts={} [{}]",
            n, stream_id, is_keyframe, batch.timestamp, desc
        );
    } else if n % 100 == 0 {
        debug!("[WebRTC] Published {} access units to '{}'", n, stream_id);
    }

    let frame = MediaFrame::new(
        stream_id.to_string(),
        0,
        batch.timestamp as u64,
        Bytes::from(combined),
        is_keyframe,
        CodecType::H264,
    );
    manager.publish_frame(frame);

    batch.parts.clear();
    batch.is_keyframe = false;
    1
}

async fn read_audio_track(
    manager: Arc<StreamManager>,
    stream_id: String,
    track: Arc<TrackRemote>,
) -> Result<()> {
    let mut frames: u64 = 0;
    while let Ok((pkt, _)) = track.read_rtp().await {
        if pkt.payload.is_empty() {
            continue;
        }
        frames += 1;
        let frame = MediaFrame::new(
            stream_id.clone(),
            1,
            pkt.header.timestamp as u64,
            Bytes::copy_from_slice(&pkt.payload),
            false,
            CodecType::Opus,
        );
        manager.publish_frame(frame);
    }
    Ok(())
}

fn store_nalu_config_from_rtp(manager: &StreamManager, stream_id: &str, payload: &[u8]) {
    let nalus = parse_rtp_h264(payload);
    let (sps, pps) = extract_sps_pps_from_nalus(&nalus);
    if let (Some(sps), Some(pps)) = (sps, pps) {
        info!(
            "[WebRTC] Stored SPS/PPS from RTP stream='{}' sps={} pps={} [{}]",
            stream_id,
            sps.len(),
            pps.len(),
            describe_rtp_payload(payload)
        );
        manager.set_stream_sps_pps(stream_id, sps, pps);
        return;
    }
    for n in &nalus {
        if n.nal_type == 7 || n.nal_type == 8 {
            manager.merge_stream_nalu_config(stream_id, &n.data);
        }
    }
}

fn h264_rtp_to_frame(
    depacketizer: &mut H264Packet,
    pkt: &Packet,
    stream_id: &str,
    manager: &StreamManager,
) -> Option<MediaFrame> {
    let payload = &pkt.payload;

    // Always extract SPS/PPS from RTP layer (STAP-A etc.)
    store_nalu_config_from_rtp(manager, stream_id, payload);

    let rtp_nalus = parse_rtp_h264(payload);
    let is_keyframe_rtp = rtp_h264::contains_idr(&rtp_nalus) || is_idr_rtp_payload(payload);

    // Depacketize for complete Annex B (handles FU-A reassembly)
    let depayload = Bytes::copy_from_slice(payload);
    let annex_b = match depacketizer.depacketize(&depayload) {
        Ok(nalu) if !nalu.is_empty() => nalu,
        Ok(_) => return None,
        Err(_) => {
            // Fallback: single-NALU / STAP-A without FU-A
            return annex_b_from_rtp_payload(payload).map(|annex_b| {
                let is_keyframe = is_keyframe_rtp || is_keyframe_annex_b(&annex_b);
                MediaFrame::new(
                    stream_id.to_string(),
                    0,
                    pkt.header.timestamp as u64,
                    annex_b,
                    is_keyframe,
                    CodecType::H264,
                )
            });
        }
    };

    let is_keyframe = is_keyframe_rtp || is_keyframe_annex_b(&annex_b);

    Some(MediaFrame::new(
        stream_id.to_string(),
        0,
        pkt.header.timestamp as u64,
        annex_b,
        is_keyframe,
        CodecType::H264,
    ))
}

pub fn wire_ice_candidates(
    pc: Arc<RTCPeerConnection>,
    ice_tx: mpsc::UnboundedSender<ServerSignal>,
) {
    pc.on_ice_candidate(Box::new(move |c| {
        let ice_tx = ice_tx.clone();
        Box::pin(async move {
            if let Some(candidate) = c {
                if let Ok(json) = candidate.to_json() {
                    debug!(
                        "[WebRTC] Outbound ICE candidate mid={:?} mline={:?}",
                        json.sdp_mid, json.sdp_mline_index
                    );
                    let _ = ice_tx.send(ServerSignal::Ice {
                        candidate: json.candidate,
                        sdp_mid: json.sdp_mid,
                        sdp_mline_index: json.sdp_mline_index,
                    });
                }
            } else {
                debug!("[WebRTC] Outbound ICE gathering complete");
            }
        })
    }));
}

pub async fn add_ice_candidate(
    pc: &Arc<RTCPeerConnection>,
    candidate: String,
    sdp_mid: Option<String>,
    sdp_mline_index: Option<u16>,
) -> Result<()> {
    use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
    debug!(
        "[WebRTC] Inbound ICE candidate mid={:?} mline={:?} cand={}",
        sdp_mid,
        sdp_mline_index,
        &candidate[..candidate.len().min(60)]
    );
    pc.add_ice_candidate(RTCIceCandidateInit {
        candidate,
        sdp_mid,
        sdp_mline_index,
        username_fragment: None,
    })
    .await
    .map_err(|e| anyhow!("add ICE candidate: {}", e))
}
