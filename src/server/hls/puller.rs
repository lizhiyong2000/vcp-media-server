/// HLS Puller - Pure Rust implementation (no ffmpeg)
/// Downloads m3u8 playlist, fetches TS segments, parses MPEG-TS to extract
/// H264/H265 video and AAC audio, publishes via StreamManager.
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, trace, warn};

use crate::core::{
    default_live_tracks, is_keyframe_annex_b, CodecType, MediaFrame, MediaTransport, StreamManager,
    StreamProtocol, StreamPuller, StreamSourceMode, TransportStatus, MILLISECOND_CLOCK_RATE,
};

use crate::server::http_flv::puller::{parse_m3u8_for_segment, resolve_relative_url};

const HLS_CONCURRENT_SEGMENTS: usize = 3;
const DECODE_BATCH_PACKETS: usize = 128;
/// ~5MB TS @ 188 bytes/packet needs ~27k packets → ~210 batches of 128.
const MAX_DECODE_BATCHES_PER_SEGMENT: usize = 512;

fn hls_segment_semaphore() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(HLS_CONCURRENT_SEGMENTS))
}

async fn decode_ts_batch(
    decoder: Arc<Mutex<super::ts_parser::TsStreamDecoder>>,
    max_packets: usize,
) -> Result<(Vec<(super::ts_parser::TsCodec, bool, Vec<u8>)>, bool)> {
    tokio::task::spawn_blocking(move || {
        let mut guard = decoder
            .lock()
            .map_err(|e| anyhow!("TS decoder lock poisoned: {e}"))?;
        if !guard.has_pending_packets() {
            return Ok((Vec::new(), false));
        }
        let before = guard.consumed_packets();
        let nals = guard.process_packets(max_packets);
        let more =
            guard.has_pending_packets() && (guard.consumed_packets() > before || !nals.is_empty());
        Ok((nals, more))
    })
    .await
    .context("TS decode worker join failed")?
}

async fn drain_decoder_until_idle(
    decoder: Arc<Mutex<super::ts_parser::TsStreamDecoder>>,
    publisher: &mut HlsSegmentPublisher<'_>,
) -> Result<usize> {
    let mut published_frames = 0usize;
    let mut idle_batches = 0u32;
    for _ in 0..MAX_DECODE_BATCHES_PER_SEGMENT {
        let has_pending = {
            let guard = decoder
                .lock()
                .map_err(|e| anyhow!("TS decoder lock poisoned: {e}"))?;
            guard.has_pending_packets()
        };
        if !has_pending {
            break;
        }
        let (nals, more) = decode_ts_batch(decoder.clone(), DECODE_BATCH_PACKETS).await?;
        for (ts_codec, is_key, nal_unit) in nals {
            publisher.publish_nal(ts_codec, is_key, &nal_unit);
            published_frames += 1;
            idle_batches = 0;
        }
        if !more {
            break;
        }
        if published_frames == 0 {
            idle_batches += 1;
            if idle_batches >= 8 {
                break;
            }
        }
        tokio::task::yield_now().await;
    }
    Ok(published_frames)
}

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
        debug!(
            "[HLS Puller] stream registered stream='{}' remote='{}'",
            local_stream_id, remote_url
        );

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
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(10))
        .user_agent("vcp-media-server/0.1")
        .build()?;

    // Download initial playlist
    debug!(
        "[HLS Puller] fetching initial playlist stream='{}' url='{}'",
        stream_id, playlist_url
    );
    let resp = client.get(&playlist_url).send().await?;
    debug!(
        "[HLS Puller] initial playlist response stream='{}' url='{}' status={} content_length={:?}",
        stream_id,
        playlist_url,
        resp.status(),
        resp.content_length()
    );
    let body = resp.text().await?;
    debug!(
        "[HLS Puller] initial playlist body stream='{}' url='{}' bytes={} lines={}",
        stream_id,
        playlist_url,
        body.len(),
        body.lines().count()
    );

    let target = parse_m3u8_for_segment(&body, &playlist_url);
    debug!(
        "[HLS Puller] initial playlist target stream='{}' target={:?}",
        stream_id, target
    );

    let (media_url, mut playlist_body) = match target {
        Some(url) if url.ends_with(".m3u8") && url != playlist_url => {
            info!("[HLS Puller] Fetching variant playlist: {}", url);
            let resp2 = client.get(&url).send().await?;
            debug!(
                "[HLS Puller] variant playlist response stream='{}' url='{}' status={} content_length={:?}",
                stream_id,
                url,
                resp2.status(),
                resp2.content_length()
            );
            let body2 = resp2.text().await?;
            debug!(
                "[HLS Puller] variant playlist body stream='{}' url='{}' bytes={} lines={}",
                stream_id,
                url,
                body2.len(),
                body2.lines().count()
            );
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
    debug!(
        "[HLS Puller] playlist codec hints stream='{}' media_url='{}' is_h265={} audio_codec_known={}",
        stream_id, media_url, is_h265, audio_codec_known
    );

    loop {
        loop_count += 1;
        debug!(
            "[HLS Puller] playlist loop start stream='{}' loop={} media_url='{}' downloaded_segments={}",
            stream_id,
            loop_count,
            media_url,
            downloaded_segments.len()
        );

        if loop_count > 600 {
            info!("[HLS Puller] Reached max loop count (10 min), stopping");
            break;
        }

        // Re-fetch the playlist
        let resp = match client.get(&media_url).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    "[HLS Puller] Failed to fetch playlist stream='{}' url='{}': {}",
                    stream_id, media_url, e
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        debug!(
            "[HLS Puller] playlist response stream='{}' loop={} status={} content_length={:?}",
            stream_id,
            loop_count,
            resp.status(),
            resp.content_length()
        );

        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    "[HLS Puller] Failed to read playlist body stream='{}' url='{}': {}",
                    stream_id, media_url, e
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        playlist_body = body.clone();

        // Parse segments — for live playlists only fetch the newest unseen segments.
        let parsed_segments = parse_media_segments(&body, &media_url);
        let segments = select_live_segments(&parsed_segments, &downloaded_segments);
        debug!(
            "[HLS Puller] playlist parsed stream='{}' loop={} body_bytes={} parsed_segments={} selected_segments={} downloaded_segments={}",
            stream_id,
            loop_count,
            body.len(),
            parsed_segments.len(),
            segments.len(),
            downloaded_segments.len()
        );

        if segments.is_empty() {
            debug!(
                "[HLS Puller] no new segments stream='{}' loop={} parsed_segments={} sleeping_ms=2000",
                stream_id,
                loop_count,
                parsed_segments.len()
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        // Download new segments (newest first)
        let mut _new_frames = 0;
        let mut playlist_expired = false;

        for seg in &segments {
            info!(
                "[HLS Puller] Downloading segment (duration={}s): {}",
                seg.duration, &seg.url
            );
            debug!(
                "[HLS Puller] segment selected stream='{}' loop={} duration={}s url='{}' timeout_ms={}",
                stream_id,
                loop_count,
                seg.duration,
                seg.url,
                segment_download_timeout(seg.duration).as_millis()
            );

            // Retry segment download up to 2 times
            let mut download_ok = false;
            let seg_timeout = segment_download_timeout(seg.duration);
            for attempt in 0..2 {
                debug!(
                    "[HLS Puller] segment attempt start stream='{}' loop={} attempt={}/2 url='{}'",
                    stream_id,
                    loop_count,
                    attempt + 1,
                    seg.url
                );
                match download_and_process_segment_streaming(
                    &client,
                    &seg.url,
                    seg_timeout,
                    &manager,
                    &stream_id,
                    &mut ts_counter_ms,
                    &mut sps_pps_stored,
                    is_h265,
                    audio_codec_known,
                )
                .await
                {
                    Ok(()) => {
                        debug!(
                            "[HLS Puller] segment attempt success stream='{}' loop={} attempt={}/2 url='{}'",
                            stream_id,
                            loop_count,
                            attempt + 1,
                            seg.url
                        );
                        download_ok = true;
                        break;
                    }
                    Err(e) => {
                        let err_msg = format!("{}", e);
                        // If 404, the playlist segment has expired — stop processing
                        // this batch and re-fetch the playlist
                        if err_msg.contains("404") {
                            warn!(
                                  "[HLS Puller] Segment 404 (playlist expired), re-fetching stream='{}' url='{}'",
                                  stream_id, &seg.url
                            );
                            playlist_expired = true;
                            break;
                        }
                        if attempt == 0 {
                            warn!(
                                  "[HLS Puller] Segment download failed stream='{}' attempt=1/2 error='{}' url='{}'",
                                  stream_id, e, &seg.url
                            );
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        } else {
                            warn!(
                                  "[HLS Puller] Segment download failed stream='{}' attempt=2/2 error='{}' url='{}'",
                                  stream_id, e, &seg.url
                            );
                        }
                    }
                }
            }

            if playlist_expired {
                break; // Stop processing stale segments, go re-fetch playlist
            }

            if !download_ok {
                // Download failed after retries, mark URL to avoid infinite retry
                downloaded_segments.insert(seg.url.clone());
                debug!(
                    "[HLS Puller] segment marked failed stream='{}' loop={} url='{}' downloaded_segments={}",
                    stream_id,
                    loop_count,
                    seg.url,
                    downloaded_segments.len()
                );
                continue;
            }

            // Only mark as downloaded after successful download + processing
            downloaded_segments.insert(seg.url.clone());
            debug!(
                "[HLS Puller] segment marked downloaded stream='{}' loop={} url='{}' downloaded_segments={}",
                stream_id,
                loop_count,
                seg.url,
                downloaded_segments.len()
            );
            _new_frames += 1;
        }

        // If playlist was expired, don't sleep full duration — re-fetch immediately
        if playlist_expired {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        }

        // Calculate sleep time based on newest segment duration
        let total_duration: f64 = segments.iter().map(|s| s.duration).sum();
        let sleep_duration = if total_duration > 1.0 {
            Duration::from_secs_f64((total_duration * 0.8).max(1.0))
        } else {
            Duration::from_secs(2)
        };

        debug!(
            "[HLS Puller] sleeping before next playlist fetch stream='{}' loop={} sleep_secs={} processed_segments={} playlist_expired={}",
            stream_id,
            loop_count,
            sleep_duration.as_secs_f64(),
            _new_frames,
            playlist_expired
        );
        tokio::time::sleep(sleep_duration).await;
    }

    let _ = manager.set_stopped(&stream_id);
    Ok(())
}

#[derive(Clone)]
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

/// Pick up to two newest segments that have not been downloaded yet.
fn select_live_segments(
    segments: &[Segment],
    downloaded: &std::collections::HashSet<String>,
) -> Vec<Segment> {
    segments
        .iter()
        .rev()
        .filter(|seg| !downloaded.contains(&seg.url))
        .take(2)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn segment_download_timeout(duration_secs: f64) -> Duration {
    // Large TS segments over slow links can take 30s+; scale with declared duration.
    let secs = (duration_secs * 12.0).ceil().max(45.0).min(120.0);
    Duration::from_secs_f64(secs)
}

async fn download_and_process_segment_streaming(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
    manager: &StreamManager,
    stream_id: &str,
    ts_counter_ms: &mut u64,
    sps_pps_stored: &mut bool,
    is_h265: bool,
    has_audio: bool,
) -> Result<()> {
    use super::ts_parser::TsStreamDecoder;

    debug!(
        "[HLS Puller] waiting segment semaphore stream='{}' url='{}' available_permits={} timeout_ms={}",
        stream_id,
        url,
        hls_segment_semaphore().available_permits(),
        timeout.as_millis()
    );
    let _segment_permit = hls_segment_semaphore()
        .acquire()
        .await
        .map_err(|_| anyhow!("HLS segment semaphore closed"))?;
    debug!(
        "[HLS Puller] acquired segment semaphore stream='{}' url='{}' available_permits={}",
        stream_id,
        url,
        hls_segment_semaphore().available_permits()
    );

    let response = client.get(url).timeout(timeout).send().await?;
    let status = response.status();
    let content_length = response.content_length();
    debug!(
        "[HLS Puller] segment response stream='{}' url='{}' status={} content_length={:?}",
        stream_id, url, status, content_length
    );

    if !status.is_success() {
        return Err(anyhow!("HTTP {} downloading segment: {}", status, url));
    }

    let decoder = Arc::new(Mutex::new(TsStreamDecoder::new(is_h265)));
    let mut publisher = HlsSegmentPublisher::new(
        manager,
        stream_id,
        ts_counter_ms,
        sps_pps_stored,
        is_h265,
        has_audio,
    );

    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    let mut total = 0usize;
    let mut published_frames = 0usize;
    let mut chunks = 0usize;
    let mut next_progress_log = 512 * 1024usize;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            anyhow!(
                "error reading segment body (timeout={}s): {} — {}",
                timeout.as_secs(),
                e,
                url
            )
        })?;
        chunks += 1;
        total += chunk.len();
        {
            let mut guard = decoder
                .lock()
                .map_err(|e| anyhow!("TS decoder lock poisoned: {e}"))?;
            guard.append(&chunk);
        }
        let before = published_frames;
        published_frames += drain_decoder_until_idle(decoder.clone(), &mut publisher).await?;
        if chunks == 1 || total >= next_progress_log || published_frames > before {
            let (raw_len, consumed, has_sync, video_pid, audio_pid, codec) = {
                let guard = decoder
                    .lock()
                    .map_err(|e| anyhow!("TS decoder lock poisoned: {e}"))?;
                (
                    guard.raw_len(),
                    guard.consumed_packets(),
                    guard.has_sync(),
                    guard.video_pid(),
                    guard.audio_pid(),
                    guard.video_codec(),
                )
            };
            debug!(
                "[HLS Puller] segment streaming progress stream='{}' url='{}' chunks={} bytes={} raw={} consumed_packets={} sync={} video_pid={:?} audio_pid={:?} ts_codec={:?} extracted_nals={} published_video={} published_audio={}",
                stream_id,
                url,
                chunks,
                total,
                raw_len,
                consumed,
                has_sync,
                video_pid,
                audio_pid,
                codec,
                published_frames,
                publisher.published_video_frames,
                publisher.published_audio_frames
            );
            while total >= next_progress_log {
                next_progress_log += 512 * 1024;
            }
        }
        publisher.ensure_tracks({
            let guard = decoder
                .lock()
                .map_err(|e| anyhow!("TS decoder lock poisoned: {e}"))?;
            guard.video_codec()
        });
        tokio::task::yield_now().await;
    }

    published_frames += drain_decoder_until_idle(decoder.clone(), &mut publisher).await?;
    debug!(
        "[HLS Puller] segment stream ended stream='{}' url='{}' chunks={} bytes={} extracted_nals={} published_video={} published_audio={}",
        stream_id,
        url,
        chunks,
        total,
        published_frames,
        publisher.published_video_frames,
        publisher.published_audio_frames
    );

    let finish_nals = {
        let mut guard = decoder
            .lock()
            .map_err(|e| anyhow!("TS decoder lock poisoned: {e}"))?;
        guard.finish()
    };
    debug!(
        "[HLS Puller] decoder finish stream='{}' url='{}' pending_nals={}",
        stream_id,
        url,
        finish_nals.len()
    );
    for (ts_codec, is_key, nal_unit) in finish_nals {
        publisher.publish_nal(ts_codec, is_key, &nal_unit);
        published_frames += 1;
    }
    publisher.ensure_tracks({
        let guard = decoder
            .lock()
            .map_err(|e| anyhow!("TS decoder lock poisoned: {e}"))?;
        guard.video_codec()
    });
    let audio_es = {
        let guard = decoder
            .lock()
            .map_err(|e| anyhow!("TS decoder lock poisoned: {e}"))?;
        guard.audio_es().to_vec()
    };
    debug!(
        "[HLS Puller] audio ES extracted stream='{}' url='{}' bytes={}",
        stream_id,
        url,
        audio_es.len()
    );
    publisher.publish_audio_es(&audio_es);

    if total < 1024 {
        debug!(
            "[HLS Puller] segment rejected too small stream='{}' url='{}' bytes={}",
            stream_id, url, total
        );
        return Err(anyhow!(
            "segment too small ({} bytes), likely truncated: {}",
            total,
            url
        ));
    }
    if published_frames == 0 {
        let (raw_len, consumed, has_sync, video_pid) = {
            let guard = decoder
                .lock()
                .map_err(|e| anyhow!("TS decoder lock poisoned: {e}"))?;
            (
                guard.raw_len(),
                guard.consumed_packets(),
                guard.has_sync(),
                guard.video_pid(),
            )
        };
        warn!(
              "[HLS Puller] No frames from segment stream='{}' raw={} consumed_packets={} sync={} video_pid={:?} url='{}'",
              stream_id, raw_len, consumed, has_sync, video_pid, url
        );
        return Err(anyhow!("no video frames extracted from segment: {}", url));
    }
    debug!(
        "[HLS Puller] segment processed stream='{}' url='{}' bytes={} chunks={} extracted_nals={} published_video={} published_audio={} final_ts_ms={} actual_codec={:?} tracks_ready={} has_keyframe={}",
        stream_id,
        url,
        total,
        chunks,
        published_frames,
        publisher.published_video_frames,
        publisher.published_audio_frames,
        *publisher.ts_counter_ms,
        publisher.actual_codec,
        publisher.tracks_ready,
        publisher.published_keyframe
    );
    Ok(())
}

async fn download_segment(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let response = client.get(url).timeout(timeout).send().await?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "HTTP {} downloading segment: {}",
            response.status(),
            url
        ));
    }

    let bytes = response.bytes().await.map_err(|e| {
        anyhow!(
            "error reading segment body (timeout={}s): {} — {}",
            timeout.as_secs(),
            e,
            url
        )
    })?;
    if bytes.len() < 1024 {
        return Err(anyhow!(
            "segment too small ({} bytes), likely truncated: {}",
            bytes.len(),
            url
        ));
    }
    Ok(bytes.to_vec())
}

struct HlsSegmentPublisher<'a> {
    manager: &'a StreamManager,
    stream_id: &'a str,
    ts_counter_ms: &'a mut u64,
    sps_pps_stored: &'a mut bool,
    is_h265_hint: bool,
    has_audio: bool,
    actual_codec: CodecType,
    h265_vps: Option<Vec<u8>>,
    h265_sps: Option<Vec<u8>>,
    h265_pps: Option<Vec<u8>>,
    h264_sps: Option<Vec<u8>>,
    h264_pps: Option<Vec<u8>>,
    tracks_ready: bool,
    published_keyframe: bool,
    published_video_frames: usize,
    published_audio_frames: usize,
    skipped_parameter_nals: usize,
}

impl<'a> HlsSegmentPublisher<'a> {
    fn new(
        manager: &'a StreamManager,
        stream_id: &'a str,
        ts_counter_ms: &'a mut u64,
        sps_pps_stored: &'a mut bool,
        is_h265_hint: bool,
        has_audio: bool,
    ) -> Self {
        Self {
            manager,
            stream_id,
            ts_counter_ms,
            sps_pps_stored,
            is_h265_hint,
            has_audio,
            actual_codec: if is_h265_hint {
                CodecType::H265
            } else {
                CodecType::H264
            },
            h265_vps: None,
            h265_sps: None,
            h265_pps: None,
            h264_sps: None,
            h264_pps: None,
            tracks_ready: false,
            published_keyframe: false,
            published_video_frames: 0,
            published_audio_frames: 0,
            skipped_parameter_nals: 0,
        }
    }

    fn ensure_tracks(&mut self, ts_codec: super::ts_parser::TsCodec) {
        if self.tracks_ready {
            return;
        }
        self.actual_codec = match ts_codec {
            super::ts_parser::TsCodec::H265 => CodecType::H265,
            super::ts_parser::TsCodec::H264 => CodecType::H264,
            _ if self.is_h265_hint => CodecType::H265,
            _ => CodecType::H264,
        };
        debug!(
            "[HLS Puller] ensure tracks stream='{}' ts_codec={:?} actual_codec={:?} has_audio={} h265_hint={}",
            self.stream_id,
            ts_codec,
            self.actual_codec,
            self.has_audio,
            self.is_h265_hint
        );
        self.manager
            .ensure_video_track_codec(self.stream_id, self.actual_codec);
        if self.has_audio {
            self.manager.ensure_aac_track(self.stream_id);
        }
        self.tracks_ready = true;
    }

    fn publish_nal(
        &mut self,
        ts_codec: super::ts_parser::TsCodec,
        _is_key_hint: bool,
        data: &[u8],
    ) {
        if matches!(ts_codec, super::ts_parser::TsCodec::Mpeg2Video) {
            debug!(
                "[HLS Puller] skip unsupported MPEG2 video frame stream='{}' bytes={}",
                self.stream_id,
                data.len()
            );
            return;
        }
        if data.len() < 5 {
            debug!(
                "[HLS Puller] skip short NAL stream='{}' ts_codec={:?} bytes={}",
                self.stream_id,
                ts_codec,
                data.len()
            );
            return;
        }
        self.ensure_tracks(ts_codec);

        let nal_header = data[4];
        let h264_type = nal_header & 0x1F;
        let h265_type = (nal_header & 0x7E) >> 1;
        let detected_codec = match ts_codec {
            super::ts_parser::TsCodec::H265 => CodecType::H265,
            super::ts_parser::TsCodec::H264 => CodecType::H264,
            super::ts_parser::TsCodec::Unknown => {
                if matches!(h265_type, 32..=34 | 19 | 20) {
                    CodecType::H265
                } else if matches!(h264_type, 7 | 8 | 5) {
                    CodecType::H264
                } else {
                    self.actual_codec
                }
            }
            super::ts_parser::TsCodec::AAC => self.actual_codec,
            super::ts_parser::TsCodec::Mpeg2Video => {
                debug!(
                    "[HLS Puller] skip unsupported MPEG2 video stream='{}' bytes={}",
                    self.stream_id,
                    data.len()
                );
                return;
            }
        };
        let previous_codec = self.actual_codec;
        self.actual_codec = detected_codec;
        let codec = detected_codec;
        if previous_codec != detected_codec {
            debug!(
                "[HLS Puller] detected codec switch stream='{}' ts_codec={:?} previous={:?} detected={:?} h264_type={} h265_type={}",
                self.stream_id,
                ts_codec,
                previous_codec,
                detected_codec,
                h264_type,
                h265_type
            );
        }

        let is_keyframe = is_keyframe_annex_b(codec, data);

        if !*self.sps_pps_stored {
            match codec {
                CodecType::H265 if h265_type == 32 => {
                    self.h265_vps = Some(data[4..].to_vec());
                    debug!(
                        "[HLS Puller] cached H265 VPS stream='{}' bytes={}",
                        self.stream_id,
                        data.len().saturating_sub(4)
                    );
                }
                CodecType::H265 if h265_type == 33 => {
                    self.h265_sps = Some(data[4..].to_vec());
                    debug!(
                        "[HLS Puller] cached H265 SPS stream='{}' bytes={}",
                        self.stream_id,
                        data.len().saturating_sub(4)
                    );
                }
                CodecType::H265 if h265_type == 34 => {
                    self.h265_pps = Some(data[4..].to_vec());
                    debug!(
                        "[HLS Puller] cached H265 PPS stream='{}' bytes={}",
                        self.stream_id,
                        data.len().saturating_sub(4)
                    );
                }
                CodecType::H264 if h264_type == 7 => {
                    self.h264_sps = Some(data[4..].to_vec());
                    debug!(
                        "[HLS Puller] cached H264 SPS stream='{}' bytes={}",
                        self.stream_id,
                        data.len().saturating_sub(4)
                    );
                }
                CodecType::H264 if h264_type == 8 => {
                    self.h264_pps = Some(data[4..].to_vec());
                    debug!(
                        "[HLS Puller] cached H264 PPS stream='{}' bytes={}",
                        self.stream_id,
                        data.len().saturating_sub(4)
                    );
                }
                _ => {}
            }

            if codec == CodecType::H265 {
                if let (Some(vps), Some(sps), Some(pps)) =
                    (&self.h265_vps, &self.h265_sps, &self.h265_pps)
                {
                    if !vps.is_empty() && !sps.is_empty() && !pps.is_empty() {
                        self.manager.set_stream_hevc_config(
                            self.stream_id,
                            vps.clone(),
                            sps.clone(),
                            pps.clone(),
                            None,
                        );
                        *self.sps_pps_stored = true;
                        debug!(
                            "[HLS Puller] stored H265 config stream='{}' vps={} sps={} pps={}",
                            self.stream_id,
                            vps.len(),
                            sps.len(),
                            pps.len()
                        );
                    }
                }
            } else if let (Some(sps), Some(pps)) = (&self.h264_sps, &self.h264_pps) {
                if !sps.is_empty() && !pps.is_empty() {
                    self.manager
                        .set_stream_sps_pps(self.stream_id, sps.clone(), pps.clone());
                    *self.sps_pps_stored = true;
                    debug!(
                        "[HLS Puller] stored H264 config stream='{}' sps={} pps={}",
                        self.stream_id,
                        sps.len(),
                        pps.len()
                    );
                }
            }
        }

        if matches!(codec, CodecType::H265 if matches!(h265_type, 32 | 33 | 34))
            || matches!(codec, CodecType::H264 if matches!(h264_type, 7 | 8))
            || h264_type == 9
            || h265_type == 35
        {
            self.skipped_parameter_nals += 1;
            trace!(
                "[HLS Puller] skip parameter/AUD NAL stream='{}' codec={:?} h264_type={} h265_type={} bytes={}",
                self.stream_id,
                codec,
                h264_type,
                h265_type,
                data.len()
            );
            if self.skipped_parameter_nals <= 3 || self.skipped_parameter_nals % 300 == 0 {
                debug!(
                    "[HLS Puller] skipped parameter/AUD NALs stream='{}' count={} last_codec={:?} last_h264_type={} last_h265_type={} last_bytes={}",
                    self.stream_id,
                    self.skipped_parameter_nals,
                    codec,
                    h264_type,
                    h265_type,
                    data.len()
                );
            }
            return;
        }

        if is_keyframe && !self.published_keyframe {
            info!(
                "[HLS Puller] Published first keyframe for stream '{}'",
                self.stream_id
            );
            self.published_keyframe = true;
        }
        if is_keyframe {
            debug!(
                "[HLS Puller] publishing keyframe stream='{}' codec={:?} ts_ms={} bytes={} h264_type={} h265_type={}",
                self.stream_id,
                codec,
                *self.ts_counter_ms,
                data.len(),
                h264_type,
                h265_type
            );
        }

        let frame = MediaFrame::new(
            self.stream_id.to_string(),
            0,
            *self.ts_counter_ms,
            Bytes::from(data.to_vec()),
            is_keyframe,
            codec,
        )
        .with_clock_rate(MILLISECOND_CLOCK_RATE);
        self.manager.publish_frame(frame);
        self.published_video_frames += 1;
        if self.published_video_frames <= 3 || self.published_video_frames % 30 == 0 {
            debug!(
                "[HLS Puller] published video frame stream='{}' codec={:?} ts_ms={} keyframe={} bytes={} count={}",
                self.stream_id,
                codec,
                *self.ts_counter_ms,
                is_keyframe,
                data.len(),
                self.published_video_frames
            );
        }
        *self.ts_counter_ms += 33;
    }

    fn publish_audio_es(&mut self, audio_es: &[u8]) {
        if audio_es.is_empty() {
            return;
        }
        use super::ts_parser;
        let mut frames = 0usize;
        for aac in ts_parser::extract_aac_frames(audio_es) {
            if aac.len() > 1 {
                let len = aac.len();
                self.manager.publish_frame(
                    MediaFrame::new(
                        self.stream_id.to_string(),
                        1,
                        *self.ts_counter_ms,
                        Bytes::from(aac),
                        false,
                        CodecType::AAC,
                    )
                    .with_clock_rate(MILLISECOND_CLOCK_RATE),
                );
                self.published_audio_frames += 1;
                frames += 1;
                if self.published_audio_frames <= 3 || self.published_audio_frames % 100 == 0 {
                    debug!(
                        "[HLS Puller] published audio frame stream='{}' ts_ms={} bytes={} total_audio={}",
                        self.stream_id,
                        *self.ts_counter_ms,
                        len,
                        self.published_audio_frames
                    );
                }
            }
        }
        debug!(
            "[HLS Puller] audio publish complete stream='{}' input_bytes={} frames={} total_audio={}",
            self.stream_id,
            audio_es.len(),
            frames,
            self.published_audio_frames
        );
    }
}

async fn process_ts_segment_hls(
    data: &[u8],
    manager: &StreamManager,
    stream_id: &str,
    ts_counter_ms: &mut u64,
    sps_pps_stored: &mut bool,
    is_h265: bool,
    has_audio: bool,
) -> Result<()> {
    use super::ts_parser::TsStreamDecoder;

    if data.len() < TS_PACKET_SIZE * 5 {
        return Err(anyhow!("segment too small: {} bytes", data.len()));
    }

    let mut decoder = TsStreamDecoder::new(is_h265);
    let mut publisher = HlsSegmentPublisher::new(
        manager,
        stream_id,
        ts_counter_ms,
        sps_pps_stored,
        is_h265,
        has_audio,
    );

    let mut published_frames = 0usize;
    for (ts_codec, is_key, nal_unit) in decoder.feed(data) {
        publisher.publish_nal(ts_codec, is_key, &nal_unit);
        published_frames += 1;
    }
    for (ts_codec, is_key, nal_unit) in decoder.finish() {
        publisher.publish_nal(ts_codec, is_key, &nal_unit);
        published_frames += 1;
    }
    publisher.ensure_tracks(decoder.video_codec());
    publisher.publish_audio_es(decoder.audio_es());

    if published_frames == 0 {
        return Err(anyhow!("no video frames extracted from segment"));
    }
    Ok(())
}

const TS_PACKET_SIZE: usize = 188;

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

    #[test]
    fn select_live_segments_prefers_newest_unseen() {
        let segments = vec![
            Segment {
                url: "a.ts".into(),
                duration: 4.0,
            },
            Segment {
                url: "b.ts".into(),
                duration: 4.0,
            },
            Segment {
                url: "c.ts".into(),
                duration: 4.0,
            },
        ];
        let mut downloaded = std::collections::HashSet::new();
        downloaded.insert("a.ts".to_string());

        let picked = select_live_segments(&segments, &downloaded);
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].url, "b.ts");
        assert_eq!(picked[1].url, "c.ts");
    }

    #[test]
    fn segment_download_timeout_scales_with_duration() {
        assert_eq!(segment_download_timeout(4.2).as_secs(), 51);
        assert_eq!(segment_download_timeout(10.0).as_secs(), 120);
    }

    #[test]
    fn h265_parameter_sets_are_stored_when_playlist_lacks_codec_hint() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Pull, StreamProtocol::HLS, None);
        let mut ts_counter_ms = 0;
        let mut sps_pps_stored = false;
        let mut publisher = HlsSegmentPublisher::new(
            &manager,
            "s",
            &mut ts_counter_ms,
            &mut sps_pps_stored,
            false,
            false,
        );

        publisher.publish_nal(
            ts_parser::TsCodec::Unknown,
            true,
            &[0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01],
        );
        publisher.publish_nal(
            ts_parser::TsCodec::Unknown,
            true,
            &[0, 0, 0, 1, 0x42, 0x01, 0x01, 0x60],
        );
        publisher.publish_nal(
            ts_parser::TsCodec::Unknown,
            true,
            &[0, 0, 0, 1, 0x44, 0x01, 0xc0, 0xf1],
        );

        let stream = manager.get_stream(&"s".to_string()).expect("stream");
        assert_eq!(stream.vps.as_deref(), Some(&[0x40, 0x01, 0x0c, 0x01][..]));
        assert_eq!(stream.sps.as_deref(), Some(&[0x42, 0x01, 0x01, 0x60][..]));
        assert_eq!(stream.pps.as_deref(), Some(&[0x44, 0x01, 0xc0, 0xf1][..]));
        assert!(sps_pps_stored);
    }

    #[test]
    fn h264_ts_codec_hint_prevents_false_h265_detection() {
        let manager = StreamManager::new();
        manager.create_stream("s", StreamSourceMode::Pull, StreamProtocol::HLS, None);
        let mut ts_counter_ms = 0;
        let mut sps_pps_stored = false;
        let mut publisher = HlsSegmentPublisher::new(
            &manager,
            "s",
            &mut ts_counter_ms,
            &mut sps_pps_stored,
            false,
            false,
        );

        publisher.publish_nal(
            ts_parser::TsCodec::H264,
            false,
            &[0, 0, 0, 1, 0x41, 0x9a, 0x22],
        );

        assert_eq!(publisher.actual_codec, CodecType::H264);
        let hub = manager.get_hub("s").expect("hub");
        let frame = hub.get(0).expect("published frame");
        assert_eq!(frame.codec, CodecType::H264);
        assert_eq!(frame.data.as_ref(), &[0, 0, 0, 1, 0x41, 0x9a, 0x22]);
    }

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
                                let data_end = std::cmp::min(data_start + section_len, 188);
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
                                println!(
                                    "[TEST] PMT parse pid={}: video={:?} audio={:?}",
                                    pid, v, a
                                );
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
                                    w == [0x00, 0x00, 0x00, 0x01] || w[..3] == [0x00, 0x00, 0x01]
                                }) {
                                    let off =
                                        if es.data[pos..].starts_with(&[0x00, 0x00, 0x00, 0x01]) {
                                            pos + 4
                                        } else {
                                            pos + 3
                                        };
                                    if off < es.data.len() {
                                        let nal_header = es.data[off];
                                        let h264_type = nal_header & 0x1F;
                                        let h265_type = (nal_header & 0x7E) >> 1;
                                        video_codec = if h265_type == 32
                                            || h265_type == 33
                                            || h265_type == 34
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
                                        j,
                                        codec,
                                        is_key,
                                        nal.len(),
                                        tag
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
        assert!(total_video_frames > 0, "Should have extracted video frames");
        println!("\n[TEST] HLS pull verification PASSED!");
    }
}
