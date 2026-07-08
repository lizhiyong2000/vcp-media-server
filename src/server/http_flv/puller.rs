/// HTTP-FLV Puller
/// Connects to an HTTP FLV source, parses FLV tags, and publishes frames via StreamManager.
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tracing::{debug, error, info, warn};

use crate::core::{
    CodecType, MILLISECOND_CLOCK_RATE, MediaFrame, MediaTransport, StreamManager, StreamProtocol,
    StreamPuller, StreamSourceMode, TransportStatus, is_keyframe_annex_b,
};

/// Simple m3u8 parser for HLS to FLV puller redirect resolution.
/// Returns the first TS segment URL or the playlist itself if it's a media playlist.
pub fn parse_m3u8_for_segment(content: &str, base_url: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().map(|s| s.trim()).collect();
    let mut is_master = false;
    let mut target_uri = None;

    // Check if master playlist
    for line in &lines {
        if line.starts_with("#EXT-X-STREAM-INF") {
            is_master = true;
            break;
        }
    }

    if is_master {
        // Find the first variant stream
        let mut best_bandwidth = u64::MAX;
        let mut current_bandwidth = 0u64;
        for line in &lines {
            if line.starts_with("#EXT-X-STREAM-INF") {
                // Parse BANDWIDTH=
                for part in line.split(',') {
                    let part = part.trim();
                    if let Some(bw_str) = part.strip_prefix("BANDWIDTH=") {
                        current_bandwidth = bw_str.parse().unwrap_or(0);
                    }
                }
            } else if !line.starts_with("#") && !line.is_empty() {
                // This is the URI following a STREAM-INF
                let uri = if line.starts_with("http") {
                    line.to_string()
                } else {
                    resolve_relative_url(base_url, line)
                };
                // We just want the first variant, or the lowest bandwidth
                // Actually for pull, let's get the variant URL and fetch its playlist
                target_uri = Some(uri);
                break;
            }
        }
    } else {
        // Media playlist - find first TS segment
        for line in &lines {
            if !line.starts_with("#") && !line.is_empty() && is_ts_url(line) {
                let uri = if line.starts_with("http") {
                    line.to_string()
                } else {
                    resolve_relative_url(base_url, line)
                };
                target_uri = Some(uri);
                break;
            }
        }
    }

    target_uri
}

pub(crate) fn resolve_relative_url(base_url: &str, path: &str) -> String {
    if path.starts_with("http") || path.starts_with("https") {
        return path.to_string();
    }
    let base = base_url
        .rsplit_once('/')
        .map(|(b, _)| b.to_string())
        .unwrap_or_else(|| base_url.to_string());
    if path.starts_with('/') {
        if let Some(host_end) = base_url.find("://") {
            let host_start = host_end + 3;
            let host_end2 = base_url[host_start..]
                .find('/')
                .map(|p| host_start + p)
                .unwrap_or(base_url.len());
            format!("{}{}", &base_url[..host_end2], path)
        } else {
            format!("{}{}", base, path)
        }
    } else {
        format!("{}/{}", base, path)
    }
}

pub(crate) fn is_m3u8_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.ends_with(".m3u8")
}

fn is_ts_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.ends_with(".ts")
}

pub struct FlvPuller {
    stream_manager: Arc<StreamManager>,
    remote_url: Option<String>,
    stream_id: String,
    peer_addr: String,
    status: TransportStatus,
}

impl FlvPuller {
    pub fn new(stream_manager: Arc<StreamManager>) -> Self {
        Self {
            stream_manager,
            remote_url: None,
            stream_id: String::new(),
            peer_addr: String::new(),
            status: TransportStatus::Idle,
        }
    }

    pub fn with_url(mut self, remote_url: &str, stream_id: &str) -> Self {
        self.remote_url = Some(remote_url.to_string());
        self.stream_id = stream_id.to_string();
        self
    }

    async fn do_pull(&self, remote_url: &str, local_stream_id: &str) -> Result<()> {
        info!("[FLV Puller] =========================================");
        info!(
            "[FLV Puller] Starting HTTP-FLV pull from {} to stream {}",
            remote_url, local_stream_id
        );
        info!("[FLV Puller] =========================================");

        // Check if URL is actually HLS (m3u8)
        let actual_url = if is_m3u8_url(remote_url) {
            info!("[FLV Puller] Detected HLS URL, resolving to first TS segment...");
            match resolve_hls_to_first_segment(remote_url).await {
                Ok(Some(seg_url)) => {
                    info!("[FLV Puller] Resolved to TS segment: {}", seg_url);
                    seg_url
                }
                Ok(None) => {
                    return Err(anyhow!("HLS playlist has no segments"));
                }
                Err(e) => {
                    return Err(anyhow!("Failed to resolve HLS: {}", e));
                }
            }
        } else {
            remote_url.to_string()
        };

        info!("[FLV Puller] Pulling from: {}", actual_url);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let response = client.get(&actual_url).send().await?;

        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!("HTTP {} from {}", status, &actual_url));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        info!(
            "[FLV Puller] Response: status={} content-type={}",
            status, content_type
        );

        // Register stream
        self.stream_manager.create_stream(
            local_stream_id,
            StreamSourceMode::Pull,
            StreamProtocol::HTTP_FLV,
            Some(remote_url.to_string()),
        );
        let _ = self.stream_manager.set_unpublished(local_stream_id);
        self.stream_manager.set_stream_broadcast(local_stream_id);
        let _ = self.stream_manager.set_publishing(local_stream_id);

        let manager = self.stream_manager.clone();
        let local_id = local_stream_id.to_string();

        // Check if content is MPEG-TS (for TS segment pull)
        if content_type.contains("video/mp2t") || actual_url.ends_with(".ts") {
            // Download entire response and parse as TS
            let bytes = response.bytes().await?;
            info!("[FLV Puller] Downloaded {} bytes of TS data", bytes.len());
            tokio::spawn(async move {
                if let Err(e) = process_ts_segment(&bytes, &manager, &local_id).await {
                    error!("[FLV Puller] TS processing error: {}", e);
                }
                let _ = manager.set_stopped(&local_id);
            });
        } else {
            // FLV streaming - read as byte stream
            tokio::spawn(async move {
                if let Err(e) = process_flv_stream(response, &manager, &local_id).await {
                    error!("[FLV Puller] Stream processing error: {}", e);
                }
                let _ = manager.set_stopped(&local_id);
            });
        }

        Ok(())
    }
}

async fn resolve_hls_to_first_segment(url: &str) -> Result<Option<String>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let response = client.get(url).send().await?;
    let body = response.text().await?;
    let target = parse_m3u8_for_segment(&body, url);
    if let Some(ref uri) = target {
        if is_m3u8_url(uri) {
            // It's a variant playlist, fetch it
            let response2 = client.get(uri).send().await?;
            let body2 = response2.text().await?;
            Ok(parse_m3u8_for_segment(&body2, uri))
        } else {
            Ok(Some(uri.clone()))
        }
    } else {
        Ok(target)
    }
}

async fn process_ts_segment(data: &[u8], manager: &StreamManager, stream_id: &str) -> Result<()> {
    use crate::server::hls::ts_parser;

    let packets = ts_parser::find_ts_packets(data);
    info!(
        "[TS Puller] Found {} TS packets in segment for stream {}",
        packets.len(),
        stream_id
    );

    if packets.len() < 10 {
        return Err(anyhow!("Too few TS packets"));
    }

    let pmt_pids = ts_parser::parse_pat(&packets);
    if pmt_pids.is_empty() {
        return Err(anyhow!("No PMT PID found"));
    }

    info!("[TS Puller] PMT PIDs: {:?}", pmt_pids);
    let pmt_info = ts_parser::parse_pmt_info(&packets, pmt_pids[0]);
    let (video_pid, audio_pid) = (pmt_info.video_pid, pmt_info.audio_pid);
    let mut video_ts_codec = pmt_info.video_codec;
    info!(
        "[TS Puller] Video PID={:?} Audio PID={:?} codec={:?}",
        video_pid, audio_pid, video_ts_codec
    );

    let mut all_video_pid = video_pid;
    let mut all_audio_pid = audio_pid;

    // Try all PMT PIDs
    if all_video_pid.is_none() {
        for &pid in &pmt_pids[1..] {
            let info = ts_parser::parse_pmt_info(&packets, pid);
            if all_video_pid.is_none() {
                all_video_pid = info.video_pid;
                video_ts_codec = info.video_codec;
            }
            if all_audio_pid.is_none() {
                all_audio_pid = info.audio_pid;
            }
        }
    }

    let video_pid = all_video_pid.ok_or_else(|| anyhow!("No video PID found"))?;
    info!(
        "[TS Puller] Using Video PID={} Audio PID={:?}",
        video_pid, all_audio_pid
    );

    let es_map = ts_parser::extract_es_data_with_codec(
        &packets,
        video_pid,
        all_audio_pid.unwrap_or(0xFFFF),
        video_ts_codec,
    );

    let mut total_frames = 0u64;
    let mut ts_counter = 0u64;
    let has_audio = es_map.contains_key(&all_audio_pid.unwrap_or(0xFFFF));

    // Detect H264 vs H265 from the actual data
    let (video_codec, actual_video_pid) = detect_video_codec(&es_map, video_pid, video_ts_codec);

    // Process video
    if let (Some(video_codec), Some(es)) = (video_codec, es_map.get(&actual_video_pid)) {
        info!(
            "[TS Puller] Video ES data: {} bytes codec={:?}",
            es.data.len(),
            video_codec
        );

        // Store codec info
        if video_codec == CodecType::H265 {
            manager.ensure_video_track_codec(stream_id, CodecType::H265);
        }

        let mut sps_found = false;
        let mut pps_found = false;
        let mut vps_found = false;

        let frames = ts_parser::extract_video_frames_with_codec(&es.data, es.codec);
        for (ts_codec, is_key, nal_unit) in &frames {
            // Detect codec from actual NAL
            let actual_codec = if *ts_codec == ts_parser::TsCodec::H265 {
                CodecType::H265
            } else {
                CodecType::H264
            };

            // Store SPS/PPS/VPS
            if actual_codec == CodecType::H265 && !nal_unit.is_empty() {
                let nal_type = (nal_unit[4] & 0x7E) >> 1;
                if nal_type == 32 {
                    let vps_bytes: Vec<u8> = nal_unit[4..].to_vec();
                    manager.set_stream_hevc_config(
                        stream_id,
                        vps_bytes,
                        Vec::new(),
                        Vec::new(),
                        None,
                    );
                    vps_found = true;
                } else if nal_type == 33 {
                    let sps_bytes: Vec<u8> = nal_unit[4..].to_vec();
                    if vps_found {
                        // Update with SPS
                        manager.set_stream_hevc_config(
                            stream_id,
                            Vec::new(), // VPS already set
                            sps_bytes,
                            Vec::new(),
                            None,
                        );
                    }
                    sps_found = true;
                } else if nal_type == 34 {
                    pps_found = true;
                }
            } else if actual_codec == CodecType::H264 && !nal_unit.is_empty() {
                let nal_type = nal_unit[4] & 0x1F;
                if nal_type == 7 {
                    let sps_bytes = nal_unit[4..].to_vec();
                    manager.set_stream_sps_pps(stream_id, sps_bytes, Vec::new());
                    sps_found = true;
                } else if nal_type == 8 && sps_found {
                    let pps_bytes = nal_unit[4..].to_vec();
                    // SPS already stored, update with PPS
                    pps_found = true;
                }
            }

            manager.publish_frame(
                MediaFrame::new(
                    stream_id.to_string(),
                    0,
                    ts_counter,
                    Bytes::from(nal_unit.clone()),
                    *is_key,
                    actual_codec,
                )
                .with_clock_rate(MILLISECOND_CLOCK_RATE),
            );
            ts_counter += 33; // approx 33ms per frame at 30fps in ms clock
            total_frames += 1;
        }
        info!(
            "[TS Puller] Published {} video frames for stream {}",
            total_frames, stream_id
        );
    } else {
        info!(
            "[TS Puller] Skip unsupported or unknown TS video stream {} pid={} ts_codec={:?}",
            stream_id, video_pid, video_ts_codec
        );
    }

    // Process audio
    if let Some(audio_pid_val) = all_audio_pid {
        if let Some(es) = es_map.get(&audio_pid_val) {
            if !es.data.is_empty() {
                manager.ensure_aac_track(stream_id);
                let aac_frames = ts_parser::extract_aac_frames(&es.data);
                let mut audio_ts = 0u64;
                for aac in &aac_frames {
                    if aac.len() > 1 {
                        manager.publish_frame(
                            MediaFrame::new(
                                stream_id.to_string(),
                                1,
                                audio_ts,
                                Bytes::from(aac.clone()),
                                false,
                                CodecType::AAC,
                            )
                            .with_clock_rate(MILLISECOND_CLOCK_RATE),
                        );
                        audio_ts += 23; // approx 23ms per AAC frame
                    }
                }
                info!(
                    "[TS Puller] Published {} AAC frames for stream {}",
                    aac_frames.len(),
                    stream_id
                );
            }
        }
    }

    Ok(())
}

fn detect_video_codec(
    es_map: &std::collections::HashMap<u16, crate::server::hls::ts_parser::TsElementaryData>,
    video_pid: u16,
    hinted_codec: crate::server::hls::ts_parser::TsCodec,
) -> (Option<CodecType>, u16) {
    match hinted_codec {
        crate::server::hls::ts_parser::TsCodec::H264 => return (Some(CodecType::H264), video_pid),
        crate::server::hls::ts_parser::TsCodec::H265 => return (Some(CodecType::H265), video_pid),
        crate::server::hls::ts_parser::TsCodec::Mpeg2Video
        | crate::server::hls::ts_parser::TsCodec::AAC => return (None, video_pid),
        crate::server::hls::ts_parser::TsCodec::Unknown => {}
    }

    if let Some(es) = es_map.get(&video_pid) {
        match es.codec {
            crate::server::hls::ts_parser::TsCodec::H264 => {
                return (Some(CodecType::H264), video_pid);
            }
            crate::server::hls::ts_parser::TsCodec::H265 => {
                return (Some(CodecType::H265), video_pid);
            }
            crate::server::hls::ts_parser::TsCodec::Mpeg2Video
            | crate::server::hls::ts_parser::TsCodec::AAC => return (None, video_pid),
            crate::server::hls::ts_parser::TsCodec::Unknown => {}
        }

        if !es.data.is_empty() {
            // Check first few bytes for H265 NAL type pattern
            if es.data.len() > 5 {
                // Look for start code prefix
                for w in es.data.windows(4) {
                    if w == [0x00, 0x00, 0x00, 0x01] || w == [0x00, 0x00, 0x01, 0x00] {
                        continue;
                    }
                }
                // Check NAL header
                if let Some(pos) = es.data.windows(5).position(|w| {
                    w[0..4] == [0x00, 0x00, 0x00, 0x01] || w[0..3] == [0x00, 0x00, 0x01]
                }) {
                    let start_pos = if es.data[pos..pos + 4] == [0x00, 0x00, 0x00, 0x01] {
                        pos + 4
                    } else {
                        pos + 3
                    };
                    if start_pos < es.data.len() {
                        let nal_header = es.data[start_pos];
                        let h264_nal_type = nal_header & 0x1F;
                        let h265_nal_type = (nal_header & 0x7E) >> 1;
                        // H265 SPS is type 33, H264 SPS is type 7
                        if h264_nal_type == 7 || h264_nal_type == 8 {
                            return (Some(CodecType::H264), video_pid);
                        }
                        if h265_nal_type == 32 || h265_nal_type == 33 || h265_nal_type == 34 {
                            return (Some(CodecType::H265), video_pid);
                        }
                    }
                }
            }
        }
    }
    (None, video_pid)
}

async fn process_flv_stream(
    response: reqwest::Response,
    manager: &StreamManager,
    stream_id: &str,
) -> Result<()> {
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;

    let mut buf = BytesMut::new();
    let mut header_parsed = false;
    let mut sps_sent = false;
    let mut pps_sent = false;
    let mut aac_seq_sent = false;
    let mut frames_received = 0u64;

    let mut prev_tag_size: u32 = 0;
    let mut current_tag_offset = 0usize;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow!("HTTP stream error: {}", e))?;
        buf.extend_from_slice(&chunk);

        // Parse FLV header if not done
        if !header_parsed && buf.len() >= 13 {
            if buf.len() < 9 {
                continue;
            }
            if &buf[0..3] != b"FLV" {
                return Err(anyhow!("Not a valid FLV stream"));
            }
            let _version = buf[3];
            let flags = buf[4];
            let has_video = (flags & 0x01) != 0;
            let has_audio = (flags & 0x04) != 0;
            let data_offset = ((buf[5] as u32) << 24)
                | ((buf[6] as u32) << 16)
                | ((buf[7] as u32) << 8)
                | buf[8] as u32;

            info!(
                "[FLV Puller] FLV header: video={} audio={} data_offset={}",
                has_video, has_audio, data_offset
            );

            // Skip to data_offset (first PreviousTagSize0 after header)
            let _ = buf.split_to(data_offset as usize);
            // PreviousTagSize0 is 4 bytes of 0
            if buf.len() >= 4 {
                let _ = buf.split_to(4);
            }
            header_parsed = true;
            current_tag_offset = 0;
        }

        // Parse FLV tags
        while buf.len() >= 11 {
            let tag_type = buf[0];
            let data_size = ((buf[1] as u32) << 16) | ((buf[2] as u32) << 8) | buf[3] as u32;
            let timestamp = ((buf[4] as u32) << 16)
                | ((buf[5] as u32) << 8)
                | buf[6] as u32
                | ((buf[7] as u32) << 24);
            let _stream_id = ((buf[8] as u32) << 16) | ((buf[9] as u32) << 8) | buf[10] as u32;

            let total_tag_size = 11 + data_size as usize + 4; // header + data + prev_tag_size

            if buf.len() < total_tag_size {
                break; // Wait for more data
            }

            let tag_data = &buf[11..11 + data_size as usize];

            match tag_type {
                0x09 => {
                    // Video tag
                    if !tag_data.is_empty() {
                        if let Some(frame) = flv_video_to_frame(tag_data, stream_id, timestamp) {
                            // Handle sequence headers
                            if tag_data.len() > 1 {
                                if let Some(enhanced) =
                                    crate::core::parse_enhanced_hevc_video_message(tag_data)
                                {
                                    if enhanced.packet_type == crate::core::RTMP_PKT_SEQUENCE_START
                                    {
                                        let (vps, sps, pps) =
                                            crate::core::parse_hevc_decoder_config_record(
                                                enhanced.payload,
                                            );
                                        if sps.is_some() {
                                            manager.ensure_video_track_codec(
                                                stream_id,
                                                CodecType::H265,
                                            );
                                            manager.set_stream_hevc_config(
                                                stream_id,
                                                vps.unwrap_or_default(),
                                                sps.unwrap_or_default(),
                                                pps.unwrap_or_default(),
                                                Some(enhanced.payload.to_vec()),
                                            );
                                            sps_sent = true;
                                            debug!("[FLV Puller] HEVC sequence header stored");
                                        }
                                        let _ = buf.split_to(total_tag_size);
                                        continue;
                                    }
                                } else {
                                    let avc_packet_type = tag_data[1];
                                    if avc_packet_type == 0x00 && tag_data.len() > 5 {
                                        // AVC sequence header
                                        let config_data = &tag_data[5..];
                                        if config_data.len() > 7 {
                                            let num_sps = (config_data[5] & 0x1F) as usize;
                                            let mut offset = 6;
                                            let mut sps_data = Vec::new();
                                            for _ in 0..num_sps {
                                                if offset + 2 > config_data.len() {
                                                    break;
                                                }
                                                let sps_len = ((config_data[offset] as usize) << 8)
                                                    | config_data[offset + 1] as usize;
                                                offset += 2;
                                                if offset + sps_len > config_data.len() {
                                                    break;
                                                }
                                                sps_data =
                                                    config_data[offset..offset + sps_len].to_vec();
                                                offset += sps_len;
                                            }
                                            let mut pps_data = Vec::new();
                                            if offset < config_data.len() {
                                                let num_pps = config_data[offset] & 0x1F;
                                                offset += 1;
                                                for _ in 0..num_pps as usize {
                                                    if offset + 2 > config_data.len() {
                                                        break;
                                                    }
                                                    let pps_len = ((config_data[offset] as usize)
                                                        << 8)
                                                        | config_data[offset + 1] as usize;
                                                    offset += 2;
                                                    if offset + pps_len > config_data.len() {
                                                        break;
                                                    }
                                                    pps_data = config_data
                                                        [offset..offset + pps_len]
                                                        .to_vec();
                                                    offset += pps_len;
                                                }
                                            }
                                            if !sps_data.is_empty() {
                                                let sps_len = sps_data.len();
                                                manager.set_stream_sps_pps(
                                                    stream_id, sps_data, pps_data,
                                                );
                                                sps_sent = true;
                                                debug!(
                                                    "[FLV Puller] AVC sequence header stored: SPS={} PPS={}",
                                                    sps_len, offset
                                                );
                                            }
                                        }
                                        let _ = buf.split_to(total_tag_size);
                                        continue;
                                    }
                                }
                            }

                            // Check if this is an enhanced HEVC tag and mark keyframe
                            let is_keyframe = if let Some(enhanced) =
                                crate::core::parse_enhanced_hevc_video_message(tag_data)
                            {
                                enhanced.is_keyframe
                            } else if !tag_data.is_empty() {
                                (tag_data[0] & 0xF0) == 0x10
                            } else {
                                false
                            };

                            let mut frame_to_publish = frame;
                            // Override keyframe flag for HEVC
                            if frame_to_publish.codec == CodecType::H265 {
                                if let Some(enhanced) =
                                    crate::core::parse_enhanced_hevc_video_message(tag_data)
                                {
                                    frame_to_publish.is_keyframe = enhanced.is_keyframe;
                                }
                            } else {
                                frame_to_publish.is_keyframe = is_keyframe;
                            }

                            manager.publish_frame(frame_to_publish);
                            frames_received += 1;
                        }
                    }
                }
                0x08 => {
                    // Audio tag
                    if !tag_data.is_empty() {
                        let audio_data =
                            if tag_data.len() > 1 && tag_data[0] == 0xAF && tag_data[1] == 0x01 {
                                // AAC raw, skip header
                                &tag_data[2..]
                            } else if tag_data.len() > 1 && tag_data[1] == 0x00 {
                                // AAC sequence header
                                if !aac_seq_sent {
                                    manager.ensure_aac_track(stream_id);
                                    aac_seq_sent = true;
                                    debug!("[FLV Puller] AAC sequence header processed");
                                }
                                let _ = buf.split_to(total_tag_size);
                                continue;
                            } else if tag_data.len() > 1 {
                                &tag_data[1..]
                            } else {
                                tag_data
                            };

                        if !audio_data.is_empty() && aac_seq_sent {
                            manager.publish_frame(
                                MediaFrame::new(
                                    stream_id.to_string(),
                                    1,
                                    timestamp as u64,
                                    Bytes::copy_from_slice(audio_data),
                                    false,
                                    CodecType::AAC,
                                )
                                .with_clock_rate(MILLISECOND_CLOCK_RATE),
                            );
                        }
                    }
                }
                0x12 => {
                    // Script data - skip
                }
                _ => {
                    // Unknown tag type
                }
            }

            let _ = buf.split_to(total_tag_size);
        }
    }

    info!(
        "[FLV Puller] Stream ended, published {} frames",
        frames_received
    );
    Ok(())
}

fn flv_video_to_frame(data: &[u8], stream_id: &str, timestamp: u32) -> Option<MediaFrame> {
    if data.is_empty() {
        return None;
    }

    // Enhanced HEVC
    if let Some(enhanced) = crate::core::parse_enhanced_hevc_video_message(data) {
        if enhanced.packet_type != crate::core::RTMP_PKT_CODED_FRAMES
            && enhanced.packet_type != crate::core::RTMP_PKT_CODED_FRAMES_X
        {
            return None;
        }
        let annex_b = crate::core::hvcc_payload_to_annex_b(enhanced.payload);
        if annex_b.is_empty() {
            return None;
        }
        let is_keyframe = enhanced.is_keyframe;
        return Some(
            MediaFrame::new(
                stream_id.to_string(),
                0,
                timestamp as u64,
                Bytes::from(annex_b),
                is_keyframe,
                CodecType::H265,
            )
            .with_clock_rate(MILLISECOND_CLOCK_RATE),
        );
    }

    // Standard AVC
    if data.len() < 2 || data[1] != 0x01 {
        return None;
    }

    let video_codec_id = data[0] & 0x0F;
    let video_codec = if video_codec_id == 0x0C {
        CodecType::H265
    } else {
        CodecType::H264
    };

    let is_keyframe = (data[0] & 0xF0) == 0x10;
    let avcc_payload = &data[5..];
    let mut annex_b = Vec::with_capacity(avcc_payload.len());
    let mut offset = 0;
    while offset + 4 <= avcc_payload.len() {
        let nalu_len = ((avcc_payload[offset] as usize) << 24)
            | ((avcc_payload[offset + 1] as usize) << 16)
            | ((avcc_payload[offset + 2] as usize) << 8)
            | (avcc_payload[offset + 3] as usize);
        offset += 4;
        if offset + nalu_len > avcc_payload.len() {
            break;
        }
        annex_b.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        annex_b.extend_from_slice(&avcc_payload[offset..offset + nalu_len]);
        offset += nalu_len;
    }

    if annex_b.is_empty() {
        return None;
    }

    let is_keyframe = is_keyframe || is_keyframe_annex_b(video_codec, &annex_b);

    Some(
        MediaFrame::new(
            stream_id.to_string(),
            0,
            timestamp as u64,
            Bytes::from(annex_b),
            is_keyframe,
            video_codec,
        )
        .with_clock_rate(MILLISECOND_CLOCK_RATE),
    )
}

#[async_trait]
impl MediaTransport for FlvPuller {
    fn protocol(&self) -> StreamProtocol {
        StreamProtocol::HTTP_FLV
    }

    fn status(&self) -> TransportStatus {
        self.status.clone()
    }

    fn peer_addr(&self) -> String {
        self.peer_addr.clone()
    }

    fn stream_id(&self) -> String {
        self.stream_id.clone()
    }

    async fn start(&mut self) -> Result<()> {
        self.status = TransportStatus::Connecting;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        self.status = TransportStatus::Closed;
        Ok(())
    }
}

#[async_trait]
impl StreamPuller for FlvPuller {
    async fn pull(&mut self) -> Result<()> {
        let url = self.remote_url.clone();
        if let Some(url) = url {
            self.status = TransportStatus::Playing;
            self.do_pull(&url, &self.stream_id).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_url_detection_accepts_query_and_fragment() {
        assert!(is_m3u8_url("http://example/live/playlist.m3u8"));
        assert!(is_m3u8_url("http://example/live/playlist.m3u8?token=1"));
        assert!(is_m3u8_url("http://example/live/playlist.m3u8#"));
    }

    #[test]
    fn media_playlist_segment_detection_accepts_query_and_fragment() {
        let playlist = "#EXTM3U\n#EXTINF:8.00\n720p/0/seg.ts#\n";

        let target = parse_m3u8_for_segment(
            playlist,
            "http://example/channels/lantian/SXxinchang2/720p.m3u8#",
        )
        .expect("segment target");

        assert_eq!(
            target,
            "http://example/channels/lantian/SXxinchang2/720p/0/seg.ts#"
        );
    }
}
