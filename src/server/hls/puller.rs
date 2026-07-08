/// HLS Puller - Pure Rust implementation (no ffmpeg)
/// Downloads m3u8 playlist, fetches TS segments, parses MPEG-TS to extract
/// H264/H265 video and AAC audio, publishes via StreamManager.
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::core::{
    default_live_tracks, ensure_annex_b, is_keyframe_annex_b, CodecType, MediaFrame,
    MediaTransport, StreamManager, StreamProtocol, StreamPuller, StreamSourceMode,
    TransportStatus, MILLISECOND_CLOCK_RATE,
};

use crate::server::http_flv::puller::{parse_m3u8_for_segment, resolve_relative_url};

pub struct HlsPuller {
    stream_manager: Arc<StreamManager>,
    remote_url: Option<String>,
    stream_id: String,
    peer_addr: String,
    status: TransportStatus,
}

impl HlsPuller {
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
        info!("[HLS Puller] =========================================");
        info!(
            "[HLS Puller] Starting HLS pull from {} to stream {}",
            remote_url, local_stream_id
        );
        info!("[HLS Puller] =========================================");

        // Register the stream
        self.stream_manager.create_stream(
            local_stream_id,
            StreamSourceMode::Pull,
            StreamProtocol::HLS,
            Some(remote_url.to_string()),
        );
        self.stream_manager
            .set_stream_tracks(local_stream_id, default_live_tracks());
        let _ = self.stream_manager.set_unpublished(local_stream_id);
        self.stream_manager.set_stream_broadcast(local_stream_id);

        let manager = self.stream_manager.clone();
        let local_id = local_stream_id.to_string();
        let url = remote_url.to_string();

        tokio::spawn(async move {
            if let Err(e) = run_hls_pull_loop(url, manager, local_id).await {
                error!("[HLS Puller] Loop error: {}", e);
            }
        });

        let _ = self.stream_manager.set_publishing(local_stream_id);
        Ok(())
    }
}

async fn run_hls_pull_loop(
    playlist_url: String,
    manager: Arc<StreamManager>,
    stream_id: String,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;

    // Download initial playlist
    let resp = client.get(&playlist_url).send().await?;
    let body = resp.text().await?;

    let target = parse_m3u8_for_segment(&body, &playlist_url);

    let (media_url, mut playlist_body) = match target {
        Some(url) if url.ends_with(".m3u8") && url != playlist_url => {
            info!("[HLS Puller] Fetching variant playlist: {}", url);
            let resp2 = client.get(&url).send().await?;
            let body2 = resp2.text().await?;
            (url, body2)
        }
        Some(_) | None => (playlist_url.clone(), body),
    };

    info!(
        "[HLS Puller] Starting segment loop for playlist: {}",
        media_url
    );

    let mut downloaded_segments: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut ts_counter_ms = 0u64;
    let mut loop_count = 0u64;
    let mut sps_pps_stored = false;

    // Determine codec info from playlist CODECS= attribute (master playlist)
    let mut is_h265 = false;
    let mut audio_codec_known = false;

    // Parse EXT-X-STREAM-INF or #EXTINF lines for codec info
    for line in playlist_body.lines() {
        if line.to_uppercase().contains("CODECS=") {
            if let Some(codec_str) = line.split("CODECS=").nth(1) {
                let codecs = codec_str.split(',').next().unwrap_or("").trim_matches('"');
                if codecs.starts_with("hvc") || codecs.starts_with("hev") {
                    is_h265 = true;
                }
            }
            // Check for audio codec
            if let Some(codec_str) = line.split("CODECS=").nth(1) {
                let parts: Vec<&str> = codec_str.split(',').collect();
                if parts.len() > 1 {
                    let audio = parts[1].trim().trim_matches('"');
                    if audio.starts_with("mp4a") {
                        audio_codec_known = true;
                    }
                }
            }
            break;
        }
    }

    loop {
        loop_count += 1;

        if loop_count > 600 {
            info!("[HLS Puller] Reached max loop count (10 min), stopping");
            break;
        }

        // Re-fetch the playlist
        let resp = match client.get(&media_url).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!("[HLS Puller] Failed to fetch playlist: {}", e);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                warn!("[HLS Puller] Failed to read playlist body: {}", e);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        playlist_body = body.clone();

        // Parse segments
        let segments = parse_media_segments(&body, &media_url);

        if segments.is_empty() {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        // Download new segments
        let mut _new_frames = 0;
        let mut playlist_expired = false;

        for seg in &segments {
            if downloaded_segments.contains(&seg.url) {
                continue;
            }

            info!(
                "[HLS Puller] Downloading segment (duration={}s): {}",
                seg.duration,
                &seg.url
            );

            // Retry segment download up to 2 times
            let mut seg_data = None;
            for attempt in 0..2 {
                match download_segment(&client, &seg.url).await {
                    Ok(d) => {
                        seg_data = Some(d);
                        break;
                    }
                    Err(e) => {
                        let err_msg = format!("{}", e);
                        // If 404, the playlist segment has expired — stop processing
                        // this batch and re-fetch the playlist
                        if err_msg.contains("404") {
                            warn!(
                                "[HLS Puller] Segment 404 (playlist expired), re-fetching: {}",
                                &seg.url
                            );
                            playlist_expired = true;
                            break;
                        }
                        if attempt == 0 {
                            warn!(
                                "[HLS Puller] Segment download failed (attempt 1/2): {} — {}",
                                e, &seg.url
                            );
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        } else {
                            warn!(
                                "[HLS Puller] Segment download failed (attempt 2/2): {} — {}",
                                e, &seg.url
                            );
                        }
                    }
                }
            }

            if playlist_expired {
                break; // Stop processing stale segments, go re-fetch playlist
            }

            let seg_data = match seg_data {
                Some(d) => d,
                None => {
                    // Download failed after retries, mark URL to avoid infinite retry
                    downloaded_segments.insert(seg.url.clone());
                    continue;
                }
            };

            // Only mark as downloaded after successful download + processing
            downloaded_segments.insert(seg.url.clone());

            if let Err(e) =
                process_ts_segment_hls(&seg_data, &manager, &stream_id, &mut ts_counter_ms, &mut sps_pps_stored, is_h265, audio_codec_known).await
            {
                warn!("[HLS Puller] Failed to process segment: {}", e);
            } else {
                _new_frames += 1;
            }
        }

        // If playlist was expired, don't sleep full duration — re-fetch immediately
        if playlist_expired {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        }

        // Calculate sleep time based on segment durations
        let total_duration: f64 = segments.iter().map(|s| s.duration).sum();
        let sleep_duration = if total_duration > 1.0 {
            Duration::from_secs_f64((total_duration * 0.8).max(1.0))
        } else {
            Duration::from_secs(2)
        };

        debug!(
            "[HLS Puller] Sleeping {}s before next playlist fetch",
            sleep_duration.as_secs_f64()
        );
        tokio::time::sleep(sleep_duration).await;
    }

    let _ = manager.set_stopped(&stream_id);
    Ok(())
}

struct Segment {
    url: String,
    duration: f64,
}

fn parse_media_segments(body: &str, base_url: &str) -> Vec<Segment> {
    let lines: Vec<&str> = body.lines().map(|l| l.trim()).collect();
    let mut segments = Vec::new();
    let mut current_duration = 0.0;

    for line in &lines {
        if line.starts_with("#EXTINF:") {
            let dur_str = line
                .trim_start_matches("#EXTINF:")
                .split(',')
                .next()
                .unwrap_or("0");
            current_duration = dur_str.parse().unwrap_or(0.0);
        } else if !line.starts_with("#") && !line.is_empty() {
            if line.ends_with(".ts") || line.contains(".ts?") {
                let url = if line.starts_with("http") {
                    line.to_string()
                } else {
                    resolve_relative_url(base_url, line)
                };
                segments.push(Segment {
                    url,
                    duration: current_duration,
                });
                current_duration = 0.0;
            }
        }
    }

    segments
}

async fn download_segment(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .timeout(Duration::from_secs(8))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "HTTP {} downloading segment: {}",
            response.status(),
            url
        ));
    }

    let bytes = response.bytes().await?;
    Ok(bytes.to_vec())
}

async fn process_ts_segment_hls(
    data: &[u8],
    manager: &StreamManager,
    stream_id: &str,
    ts_counter_ms: &mut u64,
    sps_pps_stored: &mut bool,
    is_h265: bool,
    _has_audio: bool,
) -> Result<()> {
    use super::ts_parser;

    let mut h265_vps: Option<Vec<u8>> = None;
    let mut h265_sps: Option<Vec<u8>> = None;
    let mut h265_pps: Option<Vec<u8>> = None;
    let mut h264_sps: Option<Vec<u8>> = None;
    let mut h264_pps: Option<Vec<u8>> = None;

    let packets = ts_parser::find_ts_packets(data);

    if packets.len() < 5 {
        return Err(anyhow!("Too few TS packets: {}", packets.len()));
    }

    // Find PIDs and codec from PMT
    let pmt_pids = ts_parser::parse_pat(&packets);
    if pmt_pids.is_empty() {
        return Err(anyhow!("No PAT found"));
    }

    let mut video_pid = None;
    let mut audio_pid = None;
    let mut pmt_video_codec = ts_parser::TsCodec::Unknown;
    for &pid in &pmt_pids {
        let info = ts_parser::parse_pmt_info(&packets, pid);
        if video_pid.is_none() {
            video_pid = info.video_pid;
            if info.video_codec != ts_parser::TsCodec::Unknown {
                pmt_video_codec = info.video_codec;
            }
        }
        if audio_pid.is_none() {
            audio_pid = info.audio_pid;
        }
    }

    let video_pid = video_pid.ok_or_else(|| anyhow!("No video PID found"))?;

    let es_map = ts_parser::extract_es_data(&packets, video_pid, audio_pid.unwrap_or(0xFFFF));

    let ts_video_codec = match pmt_video_codec {
        ts_parser::TsCodec::H265 => ts_parser::TsCodec::H265,
        ts_parser::TsCodec::H264 => ts_parser::TsCodec::H264,
        _ if is_h265 => ts_parser::TsCodec::H265,
        _ => ts_parser::TsCodec::H264,
    };

    let mut actual_codec = match ts_video_codec {
        ts_parser::TsCodec::H265 => CodecType::H265,
        _ => CodecType::H264,
    };

    // Detect actual codec from NAL headers when PMT is ambiguous
    if pmt_video_codec == ts_parser::TsCodec::Unknown {
        if let Some(es) = es_map.get(&video_pid) {
            let frames =
                ts_parser::extract_video_frames_with_codec(&es.data, ts_video_codec);
            for (ts_codec, _, nal_unit) in &frames {
                if nal_unit.len() < 5 {
                    continue;
                }
                let nal_header = nal_unit[4];
                let h264_type = nal_header & 0x1F;
                let h265_type = (nal_header & 0x7E) >> 1;
                if *ts_codec == ts_parser::TsCodec::H265 || (h265_type >= 32 && h265_type <= 34) {
                    actual_codec = CodecType::H265;
                    break;
                }
                if h264_type == 7 || h264_type == 8 {
                    actual_codec = CodecType::H264;
                    break;
                }
            }
        }
    }

    manager.ensure_video_track_codec(stream_id, actual_codec);
    if audio_pid.is_some() {
        manager.ensure_aac_track(stream_id);
    }

    // Process video frames
    if let Some(es) = es_map.get(&video_pid) {
        let frames =
            ts_parser::extract_video_frames_with_codec(&es.data, ts_video_codec);

        for (_ts_codec, _is_key, nal_unit) in &frames {
            let data: &[u8] = nal_unit;
            if data.len() < 5 {
                continue;
            }

            let nal_header = data[4];
            let h264_type = nal_header & 0x1F;
            let h265_type = (nal_header & 0x7E) >> 1;
            let codec = actual_codec;

            let is_keyframe = is_keyframe_annex_b(codec, data);

            // Store SPS/PPS/VPS (order-independent pairing)
            if !*sps_pps_stored {
                match codec {
                    CodecType::H265 if h265_type == 32 => {
                        h265_vps = Some(data[4..].to_vec());
                    }
                    CodecType::H265 if h265_type == 33 => {
                        h265_sps = Some(data[4..].to_vec());
                    }
                    CodecType::H265 if h265_type == 34 => {
                        h265_pps = Some(data[4..].to_vec());
                    }
                    CodecType::H264 if h264_type == 7 => {
                        h264_sps = Some(data[4..].to_vec());
                    }
                    CodecType::H264 if h264_type == 8 => {
                        h264_pps = Some(data[4..].to_vec());
                    }
                    _ => {}
                }

                if codec == CodecType::H265 {
                    if let (Some(vps), Some(sps), Some(pps)) =
                        (&h265_vps, &h265_sps, &h265_pps)
                    {
                        if !vps.is_empty() && !sps.is_empty() && !pps.is_empty() {
                            manager.set_stream_hevc_config(
                                stream_id,
                                vps.clone(),
                                sps.clone(),
                                pps.clone(),
                                None,
                            );
                            *sps_pps_stored = true;
                        }
                    }
                } else if let (Some(sps), Some(pps)) = (&h264_sps, &h264_pps) {
                    if !sps.is_empty() && !pps.is_empty() {
                        manager.set_stream_sps_pps(stream_id, sps.clone(), pps.clone());
                        *sps_pps_stored = true;
                    }
                }
            }

            // Skip parameter-set-only NALUs from the media timeline
            if matches!(codec, CodecType::H265 if matches!(h265_type, 32 | 33 | 34))
                || matches!(codec, CodecType::H264 if matches!(h264_type, 7 | 8))
                || h264_type == 9
                || h265_type == 35
            {
                continue;
            }

            let frame = MediaFrame::new(
                stream_id.to_string(),
                0,
                *ts_counter_ms,
                Bytes::from(data.to_vec()),
                is_keyframe,
                codec,
            )
            .with_clock_rate(MILLISECOND_CLOCK_RATE);

            manager.publish_frame(frame);
            *ts_counter_ms += 33;
        }
    }

    // Process audio
    if let Some(audio_pid_val) = audio_pid {
        if let Some(es) = es_map.get(&audio_pid_val) {
            if !es.data.is_empty() {
                let audio_frames = ts_parser::extract_aac_frames(&es.data);

                for aac in &audio_frames {
                    if aac.len() > 1 {
                        manager.publish_frame(
                            MediaFrame::new(
                                stream_id.to_string(),
                                1,
                                *ts_counter_ms,
                                Bytes::from(aac.clone()),
                                false,
                                CodecType::AAC,
                            )
                            .with_clock_rate(MILLISECOND_CLOCK_RATE),
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

#[async_trait]
impl MediaTransport for HlsPuller {
    fn protocol(&self) -> StreamProtocol {
        StreamProtocol::HLS
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
impl StreamPuller for HlsPuller {
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
    use crate::server::hls::ts_parser;

    const TEST_M3U8_URL: &str = "https://bp-resource-dfl.bestv.cn/148/3/video.m3u8";
    const MAX_SEGMENTS_TO_DOWNLOAD: usize = 3;

    #[tokio::test]
    async fn test_hls_pull_verify() {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build reqwest client");

        // ==================== Step 1: Fetch m3u8 playlist ====================
        println!("========================================");
        println!("[TEST] Fetching playlist: {}", TEST_M3U8_URL);
        println!("========================================");

        let resp = client
            .get(TEST_M3U8_URL)
            .send()
            .await
            .expect("Failed to fetch playlist");
        println!("[TEST] HTTP status: {}", resp.status());
        println!(
            "[TEST] Content-Type: {}",
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
        );

        let body = resp.text().await.expect("Failed to read playlist body");
        println!("[TEST] Playlist FULL content ({} bytes):", body.len());
        // Print all (not just first 500 chars)
        for line in body.lines().take(30) {
            println!("  | {}", line);
        }
        if body.lines().count() > 30 {
            println!("  ... ({} more lines)", body.lines().count() - 30);
        }

        // ==================== Step 2: Determine playlist type ====================
        let is_master = body.contains("#EXT-X-STREAM-INF");
        println!("\n[TEST] Is master playlist: {}", is_master);

        // Always parse as media segments from the fetched body
        let segments = parse_media_segments(&body, TEST_M3U8_URL);

        println!("\n[TEST] ========================================");
        println!("[TEST] Found {} segments in playlist", segments.len());
        println!("[TEST] ========================================");

        assert!(!segments.is_empty(), "No segments found in playlist");

        // Print all resolved segment URLs for diagnostic
        for (i, seg) in segments.iter().enumerate().take(5) {
            println!(
                "[TEST]   Segment {}: dur={}s url={}",
                i, seg.duration, seg.url
            );
        }

        // ==================== Step 3: Download and parse segments ====================
        let download_count = segments.len().min(MAX_SEGMENTS_TO_DOWNLOAD);
        let mut total_ts_packets = 0;
        let mut total_video_frames = 0;
        let mut total_audio_frames = 0;
        let mut video_codec = String::new();
        let mut success_count = 0;

        for (i, seg) in segments.iter().enumerate().take(download_count) {
            println!(
                "\n[TEST] --- Downloading segment {}/{} ---",
                i + 1,
                download_count
            );
            println!("[TEST] URL: {}", seg.url);

            // Use raw HTTP request to see status code
            match client.get(&seg.url).send().await {
                Ok(seg_resp) => {
                    let status = seg_resp.status();
                    let content_type = seg_resp
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("?");
                    let content_len = seg_resp
                        .headers()
                        .get("content-length")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("?");
                    println!(
                        "[TEST] Segment HTTP: {} content-type={} content-length={}",
                        status, content_type, content_len
                    );

                    if !status.is_success() {
                        println!("[TEST] WARNING: HTTP {status} - segment expired?",);
                        continue;
                    }

                    match seg_resp.bytes().await {
                        Ok(bytes) => {
                            let data = bytes.to_vec();
                            println!("[TEST] Downloaded: {} bytes", data.len());

                            // Quick check: is this TS data?
                            let sync_count = data.iter().filter(|&&b| b == 0x47).count();
                            let has_sync_at0 = !data.is_empty() && data[0] == 0x47;
                            println!(
                                "[TEST] TS sync check: first_byte=0x{:02X} sync_count={} is_ts_like={}",
                                data.get(0).unwrap_or(&0),
                                sync_count,
                                has_sync_at0 && data.len() > 50000
                            );
                            if data.len() < 1000 || (!has_sync_at0 && data.len() < 100000) {
                                println!(
                                    "[TEST] WARNING: Data doesn't look like TS, first bytes: {:02X?}",
                                    &data[..data.len().min(50)]
                                );
                                continue;
                            }

                            // Parse TS packets
                            let packets = ts_parser::find_ts_packets(&data);
                            println!("[TEST] TS packets found: {}", packets.len());
                            total_ts_packets += packets.len();

                            if packets.len() < 5 {
                                println!("[TEST] WARNING: Too few TS packets, skipping");
                                continue;
                            }

                            // Parse PAT
                            let pmt_pids = ts_parser::parse_pat(&packets);
                            println!("[TEST] PMT PIDs: {:?}", pmt_pids);

                            // DEBUG: print raw PAT packet data
                            for pkt in &packets {
                                let pid = ((pkt[1] as u16 & 0x1F) << 8) | pkt[2] as u16;
                                if pid != 0x0000 {
                                    continue;
                                }
                                let has_adapt = (pkt[3] & 0x20) != 0;
                                let mut off = 4;
                                if has_adapt {
                                    let alen = pkt[off] as usize;
                                    off += 1 + alen;
                                }
                                let pusi = (pkt[1] & 0x40) != 0;
                                if pusi && off < 188 {
                                    let ptr = pkt[off] as usize;
                                    off += 1 + ptr; // skip pointer_field to section start
                                }
                                if off + 3 > 188 {
                                    continue;
                                }
                                let table_id = pkt[off];
                                let section_len =
                                    ((pkt[off + 1] as usize & 0x0F) << 8) | pkt[off + 2] as usize;
                                let data_start = off + 3;
                                let data_end =
                                    std::cmp::min(data_start + section_len, 188);
                                println!(
                                    "[TEST] PAT raw: table_id=0x{:02X} section_len={} payload_off={} pusi={} has_adapt={}",
                                    table_id, section_len, off, pusi, has_adapt
                                );
                                if data_end > data_start {
                                    println!(
                                        "[TEST] PAT section data ({} bytes): {:02X?}",
                                        data_end - data_start,
                                        &pkt[data_start..data_end]
                                    );
                                }
                                break;
                            }

                            if pmt_pids.is_empty() {
                                println!("[TEST] WARNING: No PAT found, skipping");
                                continue;
                            }

                            // Parse PMT
                            let (mut video_pid, mut audio_pid) = (None, None);
                            for &pid in &pmt_pids {
                                let (v, a) = ts_parser::parse_pmt(&packets, pid);
                                println!("[TEST] PMT parse pid={}: video={:?} audio={:?}", pid, v, a);
                                if video_pid.is_none() {
                                    video_pid = v;
                                }
                                if audio_pid.is_none() {
                                    audio_pid = a;
                                }
                            }

                            let video_pid = match video_pid {
                                Some(pid) => pid,
                                None => {
                                    println!("[TEST] WARNING: No video PID found, skipping");
                                    continue;
                                }
                            };
                            println!(
                                "[TEST] Video PID: {}  Audio PID: {:?}",
                                video_pid, audio_pid
                            );

                            // Extract ES data
                            let es_map = ts_parser::extract_es_data(
                                &packets,
                                video_pid,
                                audio_pid.unwrap_or(0xFFFF),
                            );

                            // Process video
                            if let Some(es) = es_map.get(&video_pid) {
                                println!("[TEST] Video ES data: {} bytes", es.data.len());

                                if es.data.len() < 100 {
                                    println!(
                                        "[TEST] WARNING: Too little ES data ({}B), first bytes: {:02X?}",
                                        es.data.len(),
                                        &es.data[..es.data.len().min(20)]
                                    );
                                    continue;
                                }

                                // Detect codec from NAL header
                                if let Some(pos) = es.data.windows(4).position(|w| {
                                    w == [0x00, 0x00, 0x00, 0x01]
                                        || w[..3] == [0x00, 0x00, 0x01]
                                }) {
                                    let off = if es.data[pos..].starts_with(&[0x00, 0x00, 0x00, 0x01])
                                    {
                                        pos + 4
                                    } else {
                                        pos + 3
                                    };
                                    if off < es.data.len() {
                                        let nal_header = es.data[off];
                                        let h264_type = nal_header & 0x1F;
                                        let h265_type = (nal_header & 0x7E) >> 1;
                                        video_codec =
                                            if h265_type == 32 || h265_type == 33 || h265_type == 34
                                            {
                                                "H265".to_string()
                                            } else if h264_type == 7 || h264_type == 8 {
                                                "H264".to_string()
                                            } else {
                                                format!("Unknown(0x{:02X})", nal_header)
                                            };
                                        println!("[TEST] Detected video codec: {}", video_codec);
                                    }
                                } else {
                                    println!("[TEST] No NAL start code found in ES data");
                                    println!(
                                        "[TEST] ES first 32 bytes: {:02X?}",
                                        &es.data[..es.data.len().min(32)]
                                    );
                                }

                                let frames = ts_parser::extract_video_frames(&es.data);
                                let keyframes: Vec<_> =
                                    frames.iter().filter(|(_, k, _)| *k).collect();
                                println!(
                                    "[TEST] Video frames: {} ({} keyframes)",
                                    frames.len(),
                                    keyframes.len()
                                );
                                total_video_frames += frames.len();

                                for (j, (codec, is_key, nal)) in frames.iter().enumerate().take(3) {
                                    let tag = if nal.len() >= 5 {
                                        format!("NAL:0x{:02X}", nal[4])
                                    } else {
                                        "N/A".into()
                                    };
                                    println!(
                                        "[TEST]   Frame {}: {:?} key={} size={} {}",
                                        j, codec, is_key, nal.len(), tag
                                    );
                                }

                                if frames.is_empty() {
                                    println!(
                                        "[TEST] WARNING: extract_video_frames returned empty for {}B ES",
                                        es.data.len()
                                    );
                                }
                            } else {
                                println!("[TEST] WARNING: No video ES data in es_map");
                            }

                            // Process audio
                            if let Some(audio_pid_val) = audio_pid {
                                if let Some(es) = es_map.get(&audio_pid_val) {
                                    if !es.data.is_empty() {
                                        let aac_frames = ts_parser::extract_aac_frames(&es.data);
                                        println!(
                                            "[TEST] Audio ES data: {} bytes, {} AAC frames",
                                            es.data.len(),
                                            aac_frames.len()
                                        );
                                        total_audio_frames += aac_frames.len();
                                        for (j, aac) in aac_frames.iter().enumerate().take(2) {
                                            println!(
                                                "[TEST]   AAC frame {}: size={}B",
                                                j,
                                                aac.len()
                                            );
                                        }
                                    }
                                }
                            }

                            success_count += 1;
                        }
                        Err(e) => {
                            println!("[TEST] WARNING: Failed to read segment body: {}", e);
                        }
                    }
                }
                Err(e) => {
                    println!("[TEST] WARNING: Segment download failed: {}", e);
                }
            }
        }

        // ==================== Final summary ====================
        println!("\n[TEST] ========================================");
        println!("[TEST] SUMMARY");
        println!("[TEST] ========================================");
        println!("[TEST] Playlist URL: {}", TEST_M3U8_URL);
        println!("[TEST] Segments found: {}", segments.len());
        println!("[TEST] Segments attempted: {}", download_count);
        println!("[TEST] Segments succeeded: {}", success_count);
        println!("[TEST] Total TS packets: {}", total_ts_packets);
        println!(
            "[TEST] Video codec: {}",
            if video_codec.is_empty() {
                "Unknown"
            } else {
                &video_codec
            }
        );
        println!("[TEST] Total video frames: {}", total_video_frames);
        println!("[TEST] Total audio frames: {}", total_audio_frames);
        println!("[TEST] ========================================");

        assert!(!segments.is_empty(), "Should have found segments");
        assert!(total_ts_packets > 0, "Should have parsed TS packets");
        assert!(
            total_video_frames > 0,
            "Should have extracted video frames"
        );
        println!("\n[TEST] HLS pull verification PASSED!");
    }
}
