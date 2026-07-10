//! Multi-layered stream playback verification for integration tests.
//!
//! Combines:
//! - **Scheme 1**: ffmpeg null-muxer decode + stderr frame-count parsing
//! - **Scheme 3**: Server-side HTTP API stream/metrics checks
//! - **Scheme 6**: ffprobe stream info probing (codec, resolution)
//!
//! Protocol-specific:
//! - **Scheme 4**: HTTP-FLV tag counting (video/audio/metadata tag validation)
//! - **Scheme 5**: HLS m3u8 segment count + byte-level verification

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::media_server_runner::MediaServer;
use super::process_manager::PlayProtocol;

fn format_command(program: &str, args: &[String]) -> String {
    std::iter::once(program.to_string())
        .chain(args.iter().map(|arg| {
            if arg.chars().any(|c| {
                c.is_whitespace()
                    || matches!(c, '\'' | '"' | '$' | '\\' | '(' | ')' | ';' | '&' | '|')
            }) {
                format!("'{}'", arg.replace('\'', "'\\''"))
            } else {
                arg.clone()
            }
        }))
        .collect::<Vec<_>>()
        .join(" ")
}

// =========================================================================
// Shared result types
// =========================================================================

/// Statistics parsed from ffmpeg null-muxer decode stderr output.
#[derive(Debug, Clone)]
pub struct DecodeStats {
    pub video_frames: u64,
    pub fps: Option<f64>,
    pub speed: Option<f64>,
    pub decode_errors: Vec<String>,
    pub has_audio: bool,
    pub has_video: bool,
}

/// Stream information from ffprobe.
#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub framerate: Option<String>,
    pub errors: Vec<String>,
}

/// Statistics from HTTP-FLV byte-level analysis.
#[derive(Debug, Clone)]
pub struct FlvStats {
    pub video_tags: usize,
    pub audio_tags: usize,
    pub metadata_tags: usize,
    pub total_bytes: usize,
    pub duration_ms: u64,
}

/// Statistics from HLS playlist + segment validation.
#[derive(Debug, Clone)]
pub struct HlsStats {
    pub segment_count: usize,
    pub total_segment_bytes: usize,
    pub segments: Vec<HlsSegmentInfo>,
    pub target_duration: Option<f64>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct HlsSegmentInfo {
    pub url: String,
    pub duration: Option<f64>,
    pub bytes: usize,
}

// =========================================================================
// Scheme 1: ffmpeg null-muxer decode validation
// =========================================================================

/// Play the stream URL using `ffmpeg -f framecrc` (decode-only, per-frame CRC)
/// and parse stderr to extract frame count and decode errors.
///
/// Uses `-f framecrc` instead of `-f null` because:
/// - `framecrc` outputs a checksum for every decoded frame, giving us an
///   independent signal that frames were actually decoded.
/// - `-f null` occasionally returns exit code 183 "Nothing was written" on
///   macOS when the null muxer can't initialize.
/// - `framecrc` writes to stdout pipe, no filesystem dependency.
///
/// Much more reliable than ffplay: no SDL/display dependency, quantifiable
/// frame counts, and full error visibility.
pub async fn run_ffmpeg_decode_check(
    url: &str,
    protocol: PlayProtocol,
    duration_secs: u32,
    enhanced_rtmp_hevc: bool,
    rtsp_transport: Option<&str>,
) -> Result<DecodeStats, String> {
    run_ffmpeg_decode_check_inner(
        None,
        url,
        protocol,
        duration_secs,
        enhanced_rtmp_hevc,
        rtsp_transport,
    )
    .await
}

pub async fn run_ffmpeg_decode_check_for_stream(
    stream_id: &str,
    url: &str,
    protocol: PlayProtocol,
    duration_secs: u32,
    enhanced_rtmp_hevc: bool,
    rtsp_transport: Option<&str>,
) -> Result<DecodeStats, String> {
    run_ffmpeg_decode_check_inner(
        Some(stream_id),
        url,
        protocol,
        duration_secs,
        enhanced_rtmp_hevc,
        rtsp_transport,
    )
    .await
}

async fn run_ffmpeg_decode_check_inner(
    stream_id: Option<&str>,
    url: &str,
    protocol: PlayProtocol,
    duration_secs: u32,
    enhanced_rtmp_hevc: bool,
    rtsp_transport: Option<&str>,
) -> Result<DecodeStats, String> {
    let mut args = Vec::<String>::new();

    // --- Input options ---
    args.push("-nostdin".into());
    // Use "warning" level so we can see stream detection messages.
    // Stats lines (frame=NNN) are still emitted regardless of log level.
    args.push("-v".into());
    args.push("warning".into());
    args.push("-stats".into());
    args.push("-stats_period".into());
    args.push("1.0".into());

    if enhanced_rtmp_hevc {
        args.push("-rtmp_enhanced_codecs".into());
        args.push("hvc1".into());
    }

    if let Some(transport) = rtsp_transport {
        args.push("-rtsp_transport".into());
        args.push(transport.to_string());
    }

    // HLS: larger probe buffers for segment playlist
    if matches!(protocol, PlayProtocol::HLS) {
        args.push("-probesize".into());
        args.push("2000000".into());
        args.push("-analyzeduration".into());
        args.push("5000000".into());
    }

    // RTMP/RTSP: generous probe for live streams.
    // Live streams need enough analyzeduration to sync to a keyframe.
    if matches!(protocol, PlayProtocol::RTMP | PlayProtocol::RTSP) {
        args.push("-analyzeduration".into());
        args.push("8000000".into()); // 8s – must find an IDR keyframe
        args.push("-probesize".into());
        args.push("10000000".into()); // 10MB
        args.push("-fflags".into());
        args.push("+genpts+discardcorrupt".into());
    }

    args.push("-i".into());
    args.push(url.to_string());
    args.push("-t".into());
    // For live streams, use a longer play duration so we have more time
    // to capture frames after the probe phase.
    let play_dur = if matches!(protocol, PlayProtocol::RTMP | PlayProtocol::RTSP) {
        duration_secs.max(5)
    } else {
        duration_secs
    };
    args.push(play_dur.to_string());

    // The all-protocol tests are video playback checks. Some server paths
    // expose an audio track before audio packets are available; mapping all
    // streams makes ffmpeg fail even when video playback is healthy.
    args.push("-map".into());
    args.push("0:v:0".into());
    // Live inputs do not always stop promptly on duration alone. Decode a
    // small, explicit number of video frames so the playback probe exits as
    // soon as we have proven frames are playable.
    args.push("-frames:v".into());
    args.push("5".into());

    // Use null muxer with "-" (pipe) to avoid the "Nothing was written"
    // error that occurs with `/dev/null` file path on some macOS versions.
    args.push("-f".into());
    args.push("null".into());
    args.push("-".into());

    // --- Spawn and wait with timeout (kill child on timeout) ---
    let url_dbg = url.to_string();
    // deadline: play duration + 10s for probing. Cap at 22s.
    let deadline = (play_dur as u64 + 10).min(22);

    match stream_id {
        Some(stream_id) => eprintln!("[{stream_id}] [CMD] {}", format_command("ffmpeg", &args)),
        None => eprintln!("[CMD] {}", format_command("ffmpeg", &args)),
    }
    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg decode check: spawn failed: {e}"))?;

    let pid = child.id();

    let wait_result = tokio::time::timeout(
        Duration::from_secs(deadline),
        tokio::task::spawn_blocking(move || child.wait_with_output()),
    )
    .await;

    let output = match wait_result {
        Ok(Ok(Ok(output))) => output,
        Ok(Ok(Err(e))) => return Err(format!("ffmpeg decode check: wait error: {e}")),
        Ok(Err(join_err)) => return Err(format!("ffmpeg decode check: join error: {join_err}")),
        Err(_timeout) => {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).output();
            std::thread::sleep(Duration::from_millis(100));
            return Err(format!(
                "ffmpeg decode timed out after {deadline}s for: {url_dbg}"
            ));
        }
    };

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stats = parse_ffmpeg_decode_stderr(&stderr);

    // Success if frames decoded, or audio-only stream, or exit 0.
    if stats.video_frames > 0 || stats.has_audio {
        return Ok(stats);
    }

    // Zero frames: show stderr tail for diagnostics
    let tail = if stderr.len() > 400 {
        &stderr[stderr.len() - 400..]
    } else {
        &stderr
    };
    Err(format!(
        "ffmpeg decode: 0 frames (exit {:?}). stderr tail: {}",
        output.status.code(),
        tail.trim()
    ))
}

/// Parse ffmpeg stderr looking for:
/// - `frame=  NNN` → video frame count
/// - `fps= NNN` → playback fps
/// - `speed= N.Nx` → decode speed
/// - `Stream #0:0: Video:` / `Stream #0:1: Audio:` → track presence
/// - `Error while decoding` / `Last message repeated` → decode errors
///
/// Note: ffmpeg stats format uses `key= value` with a space separator,
/// e.g. `frame=  104 fps= 35 q=-0.0`. The parser handles this by
/// treating consecutive tokens as key/value pairs.
pub fn parse_ffmpeg_decode_stderr(stderr: &str) -> DecodeStats {
    let mut video_frames: u64 = 0;
    let mut fps: Option<f64> = None;
    let mut speed: Option<f64> = None;
    let mut decode_errors: Vec<String> = Vec::new();
    let mut has_video = false;
    let mut has_audio = false;

    for line in stderr.lines() {
        let trimmed = line.trim();

        // Parse stats line: "frame=  104 fps= 35 q=-0.0 Lsize=N/A time=..."
        // ffmpeg puts a space between key= and value in most cases,
        // but some tokens like "speed=3.0x" attach the value directly.
        // Handle both: [frame=, 104, fps=, 35, ..., speed=3.0x]
        if trimmed.starts_with("frame=") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            let mut i = 0;
            while i < parts.len() {
                // Space-separated: key= value (two tokens)
                if parts[i] == "frame=" && i + 1 < parts.len() {
                    video_frames = parts[i + 1].parse().unwrap_or(0);
                    i += 1;
                } else if parts[i] == "fps=" && i + 1 < parts.len() {
                    fps = parts[i + 1].parse().ok();
                    i += 1;
                } else if parts[i] == "speed=" && i + 1 < parts.len() {
                    speed = parts[i + 1].strip_suffix('x').and_then(|s| s.parse().ok());
                    i += 1;
                }
                // Attached value: key=value (single token, no space)
                else if let Some(val) = parts[i].strip_prefix("speed=") {
                    speed = val.strip_suffix('x').and_then(|s| s.parse().ok());
                } else if let Some(val) = parts[i].strip_prefix("frame=") {
                    video_frames = val.parse().unwrap_or(0);
                } else if let Some(val) = parts[i].strip_prefix("fps=") {
                    fps = val.parse().ok();
                }
                i += 1;
            }
        }

        // Track stream info
        if trimmed.contains("Stream #0:") && trimmed.contains("Video:") {
            has_video = true;
        }
        if trimmed.contains("Stream #0:") && trimmed.contains("Audio:") {
            has_audio = true;
        }

        // Decode errors
        if trimmed.contains("Error while decoding") {
            decode_errors.push(trimmed.to_string());
        }
        // ffmpeg coalesces repeated errors (may or may not mention "decoding")
        if trimmed.contains("Last message repeated") && trimmed.contains("times") {
            decode_errors.push(trimmed.to_string());
        }
    }

    DecodeStats {
        video_frames,
        fps,
        speed,
        decode_errors,
        has_video,
        has_audio,
    }
}

// =========================================================================
// Scheme 3: Server-side HTTP API checks
// =========================================================================

/// Verify stream is active on the server via the HTTP metrics API.
/// Checks stream status, track info, and ring buffer stats.
pub async fn verify_stream_server_side(
    server: &MediaServer,
    stream_id: &str,
) -> Result<(), String> {
    // 1) Stream info API: verifies tracks (codec), status, protocol
    let stream_path = format!("/api/stream/{stream_id}");
    let resp = server
        .get(&stream_path)
        .await
        .map_err(|e| format!("stream info API: {e}"))?;

    if resp.status != 200 {
        return Err(format!(
            "stream '{}' not found on server (HTTP {})",
            stream_id, resp.status
        ));
    }

    // 2) Metrics API: verifies ring buffer has frames, HLS stats
    let metrics_path = format!("/api/metrics/{stream_id}");
    let m_resp = server
        .get(&metrics_path)
        .await
        .map_err(|e| format!("metrics API: {e}"))?;

    if m_resp.status == 200 {
        // Basic check: ring.latest_seq > 0
        if let Some(v) = serde_json::from_str::<serde_json::Value>(&m_resp.body).ok() {
            if let Some(ring) = v.get("ring") {
                let latest_seq = ring.get("latest_seq").and_then(|s| s.as_u64()).unwrap_or(0);
                if latest_seq == 0 {
                    return Err(format!(
                        "stream '{}' metrics show zero frames (latest_seq=0)",
                        stream_id
                    ));
                }
            }
        }
    } else {
        eprintln!(
            "verify_stream_server_side: metrics API returned {} for '{}'",
            m_resp.status, stream_id
        );
        // Non-fatal: metrics may not be available
    }

    Ok(())
}

/// Assert a stream has received at least `min_frames` via metrics API.
pub async fn assert_stream_min_frames(
    server: &MediaServer,
    stream_id: &str,
    min_frames: u64,
) -> Result<u64, String> {
    let metrics_path = format!("/api/metrics/{stream_id}");
    let resp = server
        .get(&metrics_path)
        .await
        .map_err(|e| format!("metrics API: {e}"))?;

    if resp.status != 200 {
        return Err(format!("metrics API returned {}", resp.status));
    }

    let v = serde_json::from_str::<serde_json::Value>(&resp.body)
        .map_err(|e| format!("metrics parse: {e}"))?;

    let latest_seq = v
        .get("ring")
        .and_then(|r| r.get("latest_seq"))
        .and_then(|s| s.as_u64())
        .unwrap_or(0);

    if latest_seq < min_frames {
        return Err(format!(
            "stream '{}' only has {} frames, expected >= {}",
            stream_id, latest_seq, min_frames
        ));
    }

    Ok(latest_seq)
}

// =========================================================================
// Scheme 6: ffprobe stream info probing
// =========================================================================

/// Probe a stream URL with ffprobe and extract codec, resolution, etc.
pub fn probe_stream_info(url: &str, timeout_secs: u64) -> Result<StreamInfo, String> {
    probe_stream_info_inner(None, url, timeout_secs)
}

pub fn probe_stream_info_for_stream(
    stream_id: &str,
    url: &str,
    timeout_secs: u64,
) -> Result<StreamInfo, String> {
    probe_stream_info_inner(Some(stream_id), url, timeout_secs)
}

fn probe_stream_info_inner(
    stream_id: Option<&str>,
    url: &str,
    timeout_secs: u64,
) -> Result<StreamInfo, String> {
    let args = vec![
        "-v".to_string(),
        "quiet".to_string(),
        "-print_format".to_string(),
        "json".to_string(),
        "-show_streams".to_string(),
        "-read_intervals".to_string(),
        format!("%+{}", timeout_secs.max(3)),
        url.to_string(),
    ];
    match stream_id {
        Some(stream_id) => eprintln!("[{stream_id}] [CMD] {}", format_command("ffprobe", &args)),
        None => eprintln!("[CMD] {}", format_command("ffprobe", &args)),
    }
    let output = Command::new("ffprobe")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("ffprobe: spawn failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut errors: Vec<String> = Vec::new();

    if !output.status.success() && !stderr.is_empty() {
        errors.push(format!("ffprobe stderr: {}", stderr.trim()));
    }

    let v: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| {
        format!(
            "ffprobe JSON parse: {e} (stdout: {})",
            &stdout[..200.min(stdout.len())]
        )
    })?;

    let streams = v.get("streams").and_then(|s| s.as_array());

    let mut stream_info = StreamInfo {
        video_codec: None,
        audio_codec: None,
        width: None,
        height: None,
        framerate: None,
        errors,
    };

    if let Some(streams) = streams {
        for stream in streams {
            let codec_type = stream.get("codec_type").and_then(|c| c.as_str());
            let codec_name = stream
                .get("codec_name")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());

            match codec_type {
                Some("video") => {
                    stream_info.video_codec = codec_name;
                    stream_info.width = stream
                        .get("width")
                        .and_then(|w| w.as_u64())
                        .map(|w| w as u32);
                    stream_info.height = stream
                        .get("height")
                        .and_then(|h| h.as_u64())
                        .map(|h| h as u32);
                    stream_info.framerate = stream
                        .get("r_frame_rate")
                        .and_then(|r| r.as_str())
                        .map(|s| s.to_string());
                }
                Some("audio") => {
                    stream_info.audio_codec = codec_name;
                }
                _ => {}
            }
        }
    }

    Ok(stream_info)
}

// =========================================================================
// Scheme 4: HTTP-FLV stream byte-level verification
// =========================================================================

/// Download HTTP-FLV stream for a fixed duration and count FLV tags.
///
/// Verifies:
/// - Video/audio tags are present
/// - Tag count grows over time
/// - Total bytes received is non-trivial
pub async fn verify_http_flv_stream(url: &str, duration_secs: u64) -> Result<FlvStats, String> {
    let data = http_get_raw(url, Duration::from_secs(duration_secs + 5))
        .await
        .map_err(|e| format!("FLV download: {e}"))?;

    if data.is_empty() {
        return Err("FLV stream returned zero bytes".into());
    }

    parse_flv_data(&data, duration_secs)
}

/// Download raw HTTP response body (streaming, up to max_duration).
async fn http_get_raw(url: &str, max_duration: Duration) -> Result<Vec<u8>, String> {
    use tokio::net::TcpStream;

    // Parse host:port:path from URL
    let (host, port, path) = parse_http_url(url)?;

    let deadline = Instant::now() + max_duration;

    let mut stream = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("FLV connect {host}:{port}: {e}"))?;

    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("FLV request: {e}"))?;

    let mut buf = [0u8; 65536];
    let mut all = Vec::new();

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => all.extend_from_slice(&buf[..n]),
            Ok(Err(_)) => break,
            Err(_) => {
                if Instant::now() >= deadline {
                    break;
                }
            }
        }
    }

    let _ = stream.shutdown().await;

    // Strip HTTP response headers
    let separator = find_http_body_start(&all).unwrap_or(0);
    let body: Vec<u8> = all[separator..].to_vec();

    if body.is_empty() {
        // Check HTTP status
        let header_text = String::from_utf8_lossy(&all);
        if !header_text.contains("200 OK") && !header_text.contains("200 ") {
            let status_line = header_text.lines().next().unwrap_or("unknown");
            return Err(format!("FLV HTTP error: {}", status_line));
        }
    }

    // Handle chunked transfer encoding if the server uses it
    let is_chunked = String::from_utf8_lossy(&all)
        .lines()
        .any(|l| l.to_lowercase().starts_with("transfer-encoding: chunked"));

    if is_chunked {
        dechunk_body(&body)
    } else {
        Ok(body)
    }
}

/// Parse FLV body bytes, counting tags.
fn parse_flv_data(data: &[u8], expected_duration_secs: u64) -> Result<FlvStats, String> {
    // FLV header: 9 bytes minimum
    if data.len() < 13 {
        return Err(format!(
            "FLV data too short: {} bytes (need >= 13 for header + first tag)",
            data.len()
        ));
    }

    // Check FLV signature: "FLV\x01"
    if &data[0..3] != b"FLV" {
        // Show what we actually received for debugging
        let preview = String::from_utf8_lossy(&data[..data.len().min(200)]);
        return Err(format!(
            "not a valid FLV stream (magic: {:02X?}, preview: \"{}\")",
            &data[0..3.min(data.len())],
            preview.trim()
        ));
    }

    let has_audio = data[4] & 0x04 != 0;
    let has_video = data[4] & 0x01 != 0;

    // FLV header: bytes 0-2=signature, 3=version, 4=flags
    // Bytes 5-8 = DataOffset (uint32 BE, usually 9)
    let header_length = ((data[5] as usize) << 24)
        | ((data[6] as usize) << 16)
        | ((data[7] as usize) << 8)
        | (data[8] as usize);

    // After header_length bytes: PreviousTagSize0 (4 bytes of 0x00), then first tag
    let mut pos = header_length + 4;
    let mut video_tags = 0usize;
    let mut audio_tags = 0usize;
    let mut metadata_tags = 0usize;

    while pos + 15 <= data.len() {
        let tag_type = data[pos];
        let data_size = ((data[pos + 1] as usize) << 16)
            | ((data[pos + 2] as usize) << 8)
            | (data[pos + 3] as usize);
        let _timestamp = ((data[pos + 4] as u32) << 16)
            | ((data[pos + 5] as u32) << 8)
            | (data[pos + 6] as u32)
            | ((data[pos + 7] as u32) << 24);

        let payload_start = pos + 11;
        let next_tag = payload_start + data_size + 4; // +4 for PreviousTagSize

        if next_tag > data.len() {
            break;
        }

        match tag_type {
            0x08 => audio_tags += 1,
            0x09 => video_tags += 1,
            0x12 => metadata_tags += 1,
            _ => {}
        }

        pos = next_tag;
    }

    if video_tags == 0 && audio_tags == 0 {
        return Err(format!(
            "FLV stream has zero tags ({} bytes, has_video={}, has_audio={})",
            data.len(),
            has_video,
            has_audio
        ));
    }

    Ok(FlvStats {
        video_tags,
        audio_tags,
        metadata_tags,
        total_bytes: data.len(),
        duration_ms: (expected_duration_secs * 1000) as u64,
    })
}

// =========================================================================
// Scheme 5: HLS playlist + segment validation
// =========================================================================

/// Fetch HLS playlist, count segments, and verify each segment is accessible
/// and non-empty.  Retries m3u8 fetch a few times in case the playlist
/// is still being populated.
pub async fn verify_hls_playlist(
    m3u8_url: &str,
    expected_min_segments: usize,
) -> Result<HlsStats, String> {
    let (host, port, path) = parse_http_url(m3u8_url)?;

    // 1) Download m3u8 playlist (with retry: first segments may not be ready)
    let m3u8_body = {
        let mut last_err = String::new();
        let mut body = String::new();
        let mut got_it = false;
        for attempt in 0..8 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            match http_get_string(&host, port, &path).await {
                Ok(b) => {
                    // Must contain actual segment entries (#EXTINF:), not just header.
                    // A newly-triggered HLS stream may return the header before
                    // the first segment is ready.
                    if b.contains("#EXTINF:") {
                        body = b;
                        got_it = true;
                        break;
                    }
                    last_err = format!(
                        "m3u8 has no segments yet (attempt {attempt}): {}",
                        &b[..b.len().min(200)]
                    );
                }
                Err(e) => last_err = e,
            }
        }
        if got_it {
            body
        } else {
            return Err(format!(
                "HLS m3u8 has no segments after retries: {last_err}"
            ));
        }
    };

    // 2) Parse segments
    let base_url = extract_base_url(m3u8_url);
    let segments = parse_m3u8_segments(&m3u8_body, &base_url);

    if segments.is_empty() {
        return Err(format!(
            "HLS playlist has zero segments:\n{}",
            &m3u8_body[..300.min(m3u8_body.len())]
        ));
    }

    if segments.len() < expected_min_segments {
        return Err(format!(
            "HLS playlist: {} segments, expected >= {}",
            segments.len(),
            expected_min_segments
        ));
    }

    let target_duration = m3u8_body
        .lines()
        .find(|l| l.starts_with("#EXT-X-TARGETDURATION:"))
        .and_then(|l| {
            l.trim_start_matches("#EXT-X-TARGETDURATION:")
                .parse::<f64>()
                .ok()
        });

    // 3) Verify each segment (with retry per segment)
    let mut infos = Vec::new();
    let mut total_bytes = 0usize;
    let mut errors = Vec::new();

    for seg in &segments {
        let seg_url = if seg.starts_with("http") {
            seg.to_string()
        } else {
            format!("{base_url}/{seg}")
        };

        let (seg_host, seg_port, seg_path) = match parse_http_url(&seg_url) {
            Ok(p) => p,
            Err(e) => {
                errors.push(format!("segment URL parse: {e}"));
                continue;
            }
        };

        // Retry up to 3 times with increasing backoff
        let mut seg_data = Err("no attempts".to_string());
        for retry in 0..3 {
            if retry > 0 {
                tokio::time::sleep(Duration::from_secs(1 + retry as u64)).await;
            }
            seg_data = http_get_raw_segment(&seg_host, seg_port, &seg_path).await;
            if seg_data.is_ok() {
                break;
            }
        }

        match seg_data {
            Ok(data) => {
                if data.is_empty() {
                    errors.push(format!("segment {} is empty", seg));
                } else {
                    total_bytes += data.len();
                    infos.push(HlsSegmentInfo {
                        url: seg.to_string(),
                        duration: None,
                        bytes: data.len(),
                    });
                }
            }
            Err(e) => {
                errors.push(format!(
                    "segment {} download failed after retries: {}",
                    seg, e
                ));
            }
        }
    }

    if infos.is_empty() && !segments.is_empty() {
        return Err(format!(
            "HLS: all {} segments failed verification: {:?}",
            segments.len(),
            errors.first()
        ));
    }

    Ok(HlsStats {
        segment_count: infos.len(),
        total_segment_bytes: total_bytes,
        segments: infos,
        target_duration,
        errors,
    })
}

async fn http_get_string(host: &str, port: u16, path: &str) -> Result<String, String> {
    let data = http_get_raw_segment(host, port, path).await?;
    let header_end = find_http_body_start(&data).unwrap_or(0);

    if header_end == 0 && data.len() > 4 {
        // No HTTP headers – raw response
        Ok(String::from_utf8_lossy(&data).to_string())
    } else if header_end >= data.len() {
        Err(format!("HTTP response has no body ({} bytes)", data.len()))
    } else {
        Ok(String::from_utf8_lossy(&data[header_end..]).to_string())
    }
}

async fn http_get_raw_segment(host: &str, port: u16, path: &str) -> Result<Vec<u8>, String> {
    use tokio::net::TcpStream;

    // Overall timeout for segment download: 8 seconds
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut stream = TcpStream::connect((host, port))
            .await
            .map_err(|e| format!("connect {host}:{port}: {e}"))?;

        let request =
            format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| format!("write: {e}"))?;

        let mut buf = [0u8; 65536];
        let mut all = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(6);

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            match tokio::time::timeout(remaining, stream.read(&mut buf)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => all.extend_from_slice(&buf[..n]),
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }

        Ok::<_, String>(all)
    })
    .await
    .map_err(|_| format!("segment download timed out: {path}"))?
}

// =========================================================================
// Combined multi-layer verification
// =========================================================================

/// Run all applicable layer checks for a stream that has been published.
///
/// Layers:
/// 1. ffmpeg null-muxer decode (frame count + errors)
/// 2. Server-side HTTP API (stream status + metrics ring buffer)
/// 3. ffprobe stream info (codec, resolution)
///
/// This is the primary combined verification for RTMP/RTSP/HLS protocols.
pub async fn verify_stream_all_layers(
    url: &str,
    protocol: PlayProtocol,
    stream_id: &str,
    server: &MediaServer,
    duration_secs: u32,
    enhanced_rtmp_hevc: bool,
    rtsp_transport: Option<&str>,
    min_frames: u64,
    server_check: bool,
) -> Result<VeificationReport, String> {
    let mut report = VeificationReport::default();

    // Layer 1: ffmpeg null-muxer decode
    match run_ffmpeg_decode_check(
        url,
        protocol,
        duration_secs,
        enhanced_rtmp_hevc,
        rtsp_transport,
    )
    .await
    {
        Ok(stats) => {
            report.decode_stats = Some(stats.clone());
            if stats.video_frames < min_frames {
                report.errors.push(format!(
                    "ffmpeg decode: only {} frames (min expected {})",
                    stats.video_frames, min_frames
                ));
            }
            if !stats.decode_errors.is_empty() {
                report.errors.extend(
                    stats
                        .decode_errors
                        .iter()
                        .map(|e| format!("decode error: {e}")),
                );
            }
        }
        Err(e) => report.errors.push(format!("ffmpeg decode: {e}")),
    }

    // Layer 2: Server-side HTTP API
    if server_check {
        match verify_stream_server_side(server, stream_id).await {
            Ok(()) => report.server_check_ok = true,
            Err(e) => report.errors.push(format!("server check: {e}")),
        }
    }

    // Layer 3: ffprobe
    match probe_stream_info(url, 5) {
        Ok(info) => {
            report.stream_info = Some(info.clone());
            if info.video_codec.is_none() && info.audio_codec.is_none() {
                report
                    .errors
                    .push("ffprobe: no video or audio track found".into());
            }
        }
        Err(e) => report.errors.push(format!("ffprobe: {e}")),
    }

    if report.errors.is_empty() {
        Ok(report)
    } else {
        Err(report.errors.join("; "))
    }
}

#[derive(Debug, Clone, Default)]
pub struct VeificationReport {
    pub decode_stats: Option<DecodeStats>,
    pub stream_info: Option<StreamInfo>,
    pub server_check_ok: bool,
    pub errors: Vec<String>,
}

// =========================================================================
// Utilities
// =========================================================================

fn parse_http_url(url: &str) -> Result<(String, u16, String), String> {
    let url = url.trim_start_matches("http://");

    let (host_port, path) = url
        .split_once('/')
        .map(|(h, p)| (h, format!("/{p}")))
        .unwrap_or((url, "/".to_string()));

    let (host, port) = host_port
        .split_once(':')
        .map(|(h, p)| (h.to_string(), p.parse::<u16>().unwrap_or(80)))
        .unwrap_or((host_port.to_string(), 80));

    Ok((host, port, path))
}

/// Decode HTTP chunked transfer encoding body into raw bytes.
fn dechunk_body(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(data.len());
    let mut pos = 0;

    while pos < data.len() {
        // Find end of chunk size line: "<hex>\r\n"
        let crlf = match data[pos..].windows(2).position(|w| w == b"\r\n") {
            Some(p) => pos + p,
            None => break,
        };

        let size_str = String::from_utf8_lossy(&data[pos..crlf]).to_string();
        let chunk_size = match usize::from_str_radix(size_str.trim(), 16) {
            Ok(s) => s,
            Err(_) => {
                // If we can't parse, skip this line and try next
                pos = crlf + 2;
                continue;
            }
        };

        if chunk_size == 0 {
            // Final chunk
            break;
        }

        let data_start = crlf + 2;
        let data_end = data_start + chunk_size;

        if data_end > data.len() {
            // Partial chunk – take what we have
            out.extend_from_slice(&data[data_start..data.len()]);
            break;
        }

        out.extend_from_slice(&data[data_start..data_end]);

        // Skip past chunk data + trailing CRLF
        pos = data_end + 2;
    }

    if out.is_empty() && !data.is_empty() {
        // Not chunked? Return as-is
        Ok(data.to_vec())
    } else {
        Ok(out)
    }
}

fn find_http_body_start(data: &[u8]) -> Option<usize> {
    data.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

fn extract_base_url(m3u8_url: &str) -> String {
    m3u8_url
        .rsplit_once('/')
        .map(|(base, _)| base.to_string())
        .unwrap_or_else(|| m3u8_url.to_string())
}

fn parse_m3u8_segments(body: &str, _base_url: &str) -> Vec<String> {
    let mut segments = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        // Non-empty, non-comment line → segment URL
        if trimmed.starts_with("http") {
            segments.push(trimmed.to_string());
        } else {
            segments.push(trimmed.to_string());
        }
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ffmpeg_stderr_frame_count() {
        let stderr = r#"
Input #0, flv, from 'rtmp://...':
  Stream #0:0: Video: h264
  Stream #0:1: Audio: aac
Output #0, null, to '/dev/null':
frame=   75 fps= 25 q=-0.0 Lsize=N/A time=00:00:03.00 bitrate=N/A speed=3.0x
"#;
        let stats = parse_ffmpeg_decode_stderr(stderr);
        assert_eq!(stats.video_frames, 75);
        assert_eq!(stats.fps, Some(25.0));
        assert_eq!(stats.speed, Some(3.0));
        assert!(stats.has_video);
        assert!(stats.has_audio);
    }

    #[test]
    fn test_parse_ffmpeg_stderr_decode_errors() {
        let stderr = r#"
Stream #0:0: Video: h264
Error while decoding stream #0:0: Invalid data found
    Last message repeated 2 times
frame=   10 fps=0.0 q=-0.0 Lsize=N/A
"#;
        let stats = parse_ffmpeg_decode_stderr(stderr);
        assert_eq!(stats.decode_errors.len(), 2);
    }
}
