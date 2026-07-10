//! Machine-readable live latency probe for integration tests.
//!
//! The publisher generates RGB frames with a timestamp marker encoded as large
//! black/white blocks in the top-left corner. Playback is decoded back to RGB
//! and the marker is read from pixels, avoiding OCR and display dependencies.

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncReadExt;
use tokio::process::Command as TokioCommand;

use super::process_manager::{format_command, stop_child, PlayProtocol, PublishCodec};

const MARKER_MAGIC: u16 = 0x5A3C;
const MARKER_BYTES: usize = 14;
const MARKER_BITS: usize = MARKER_BYTES * 8;
const MARKER_COLS: usize = 16;
const MARKER_BLOCK: usize = 10;
const MARKER_X: usize = 8;
const MARKER_Y: usize = 8;

#[derive(Debug, Clone)]
pub struct RealtimeProbeConfig {
    pub width: usize,
    pub height: usize,
    pub fps: u32,
    pub measure_duration: Duration,
    pub frames_to_sample: usize,
    pub max_startup_latency: Duration,
    pub max_p95_latency: Duration,
    pub max_latency: Duration,
    pub max_dropped_frames: Option<u64>,
}

impl Default for RealtimeProbeConfig {
    fn default() -> Self {
        Self {
            width: 320,
            height: 240,
            fps: 15,
            measure_duration: Duration::from_secs(120),
            frames_to_sample: 15 * 120 * 8 / 10,
            max_startup_latency: Duration::from_millis(5000),
            max_p95_latency: Duration::from_millis(3500),
            max_latency: Duration::from_millis(5000),
            max_dropped_frames: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RealtimeStats {
    pub frames: usize,
    pub startup_latency_ms: u64,
    pub min_latency_ms: u64,
    pub p50_latency_ms: u64,
    pub p95_latency_ms: u64,
    pub max_latency_ms: u64,
    pub dropped_frames: u64,
    pub invalid_marker_frames: u64,
}

pub struct TimestampVideoPublisher {
    child: Child,
    stop: Arc<AtomicBool>,
    writer: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone)]
pub struct TimestampPublisherConfig {
    pub url: String,
    pub codec: PublishCodec,
    pub rtmp_enhanced_hevc: bool,
    pub rtsp_transport: Option<&'static str>,
}

impl TimestampVideoPublisher {
    pub fn start(
        stream_id: &str,
        publisher: &TimestampPublisherConfig,
        config: &RealtimeProbeConfig,
    ) -> Result<Self, String> {
        let args = publish_args(publisher, config);
        eprintln!("[{stream_id}] [CMD] {}", format_command("ffmpeg", &args));

        let mut child = Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn timestamp publisher ffmpeg: {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "timestamp publisher stdin unavailable".to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let writer = spawn_frame_writer(stdin, Arc::clone(&stop), config.clone());

        Ok(Self {
            child,
            stop,
            writer: Some(writer),
        })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        let _ = stop_child(&mut self.child);
    }
}

impl Drop for TimestampVideoPublisher {
    fn drop(&mut self) {
        self.stop();
    }
}

pub async fn measure_realtime_playback(
    stream_id: &str,
    url: &str,
    protocol: PlayProtocol,
    config: &RealtimeProbeConfig,
    rtsp_transport: Option<&str>,
    enhanced_rtmp_hevc: bool,
) -> Result<RealtimeStats, String> {
    let mut args = Vec::<String>::new();
    args.extend(["-nostdin".into(), "-v".into(), "error".into()]);

    if enhanced_rtmp_hevc {
        args.push("-rtmp_enhanced_codecs".into());
        args.push("hvc1".into());
    }

    if matches!(protocol, PlayProtocol::HLS) {
        args.extend([
            "-live_start_index".into(),
            "-1".into(),
            "-analyzeduration".into(),
            "5000000".into(),
            "-probesize".into(),
            "2000000".into(),
        ]);
    } else {
        args.extend([
            "-fflags".into(),
            "nobuffer".into(),
            "-flags".into(),
            "low_delay".into(),
            "-analyzeduration".into(),
            "1000000".into(),
            "-probesize".into(),
            "1000000".into(),
        ]);
    }

    if let Some(transport) = rtsp_transport {
        args.push("-rtsp_transport".into());
        args.push(transport.to_string());
    }

    args.extend([
        "-i".into(),
        url.to_string(),
        "-an".into(),
        "-map".into(),
        "0:v:0".into(),
        "-pix_fmt".into(),
        "rgb24".into(),
        "-f".into(),
        "rawvideo".into(),
        "pipe:1".into(),
    ]);

    eprintln!("[{stream_id}] [CMD] {}", format_command("ffmpeg", &args));

    let pull_started = Instant::now();
    let mut child = TokioCommand::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn realtime playback ffmpeg: {e}"))?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "realtime playback stdout unavailable".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "realtime playback stderr unavailable".to_string())?;

    let frame_size = config.width * config.height * 3;
    let mut frame = vec![0u8; frame_size];
    let mut samples = Vec::<(u64, u32)>::new();
    let mut invalid_marker_frames = 0u64;
    let mut first_marker_at = None;
    let deadline = Instant::now() + config.measure_duration;

    while Instant::now() < deadline {
        let read_result = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            stdout.read_exact(&mut frame),
        )
        .await;

        match read_result {
            Ok(Ok(_)) => {
                if let Some(marker) = decode_marker(&frame, config.width, config.height) {
                    let decoded_at_ms = now_ms();
                    if first_marker_at.is_none() {
                        first_marker_at = Some(Instant::now());
                    }
                    samples.push((
                        decoded_at_ms.saturating_sub(marker.source_wall_ms),
                        marker.seq,
                    ));
                } else {
                    invalid_marker_frames += 1;
                }
            }
            Ok(Err(e)) => {
                let mut err = String::new();
                let _ = stderr.read_to_string(&mut err).await;
                let _ = child.kill().await;
                return Err(format!(
                    "realtime playback ended before enough frames: {e}; {err}"
                ));
            }
            Err(_) => break,
        }
    }

    let _ = child.kill().await;
    let _ = child.wait().await;

    if samples.len() < config.frames_to_sample {
        let mut err = String::new();
        let _ = stderr.read_to_string(&mut err).await;
        return Err(format!(
            "decoded too few timestamped frames: got {}, expected at least {}, invalid {}, stderr: {}",
            samples.len(),
            config.frames_to_sample,
            invalid_marker_frames,
            err
        ));
    }

    let startup_latency_ms = first_marker_at
        .map(|at| at.duration_since(pull_started).as_millis() as u64)
        .unwrap_or(u64::MAX);
    let mut latencies = samples
        .iter()
        .map(|(latency_ms, _)| *latency_ms)
        .collect::<Vec<_>>();
    latencies.sort_unstable();

    let dropped_frames = samples
        .windows(2)
        .map(|pair| {
            let prev = pair[0].1;
            let next = pair[1].1;
            next.saturating_sub(prev).saturating_sub(1) as u64
        })
        .sum();

    Ok(RealtimeStats {
        frames: samples.len(),
        startup_latency_ms,
        min_latency_ms: percentile(&latencies, 0),
        p50_latency_ms: percentile(&latencies, 50),
        p95_latency_ms: percentile(&latencies, 95),
        max_latency_ms: percentile(&latencies, 100),
        dropped_frames,
        invalid_marker_frames,
    })
}

pub fn assert_realtime_stats(
    stream_id: &str,
    protocol_label: &str,
    stats: &RealtimeStats,
    config: &RealtimeProbeConfig,
) -> Result<(), String> {
    eprintln!(
        "[{stream_id}] [REALTIME] {protocol_label} frames={} startup={}ms latency[min/p50/p95/max]={}/{}/{}/{}ms dropped={} invalid={}",
        stats.frames,
        stats.startup_latency_ms,
        stats.min_latency_ms,
        stats.p50_latency_ms,
        stats.p95_latency_ms,
        stats.max_latency_ms,
        stats.dropped_frames,
        stats.invalid_marker_frames
    );

    if stats.startup_latency_ms > config.max_startup_latency.as_millis() as u64 {
        return Err(format!(
            "{protocol_label} startup latency too high: {}ms > {}ms",
            stats.startup_latency_ms,
            config.max_startup_latency.as_millis()
        ));
    }

    if stats.p95_latency_ms > config.max_p95_latency.as_millis() as u64 {
        return Err(format!(
            "{protocol_label} p95 live latency too high: {}ms > {}ms",
            stats.p95_latency_ms,
            config.max_p95_latency.as_millis()
        ));
    }

    if stats.max_latency_ms > config.max_latency.as_millis() as u64 {
        return Err(format!(
            "{protocol_label} max live latency too high: {}ms > {}ms",
            stats.max_latency_ms,
            config.max_latency.as_millis()
        ));
    }

    let max_allowed_drops = config
        .max_dropped_frames
        .unwrap_or_else(|| (config.frames_to_sample / 4).max(1) as u64);
    if stats.dropped_frames > max_allowed_drops {
        return Err(format!(
            "{protocol_label} dropped too many marker frames: {} > {}",
            stats.dropped_frames, max_allowed_drops
        ));
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct FrameMarker {
    source_wall_ms: u64,
    seq: u32,
}

fn publish_args(publisher: &TimestampPublisherConfig, config: &RealtimeProbeConfig) -> Vec<String> {
    let codec_name = match publisher.codec {
        PublishCodec::H264 => "libx264",
        PublishCodec::H265 => "libx265",
    };
    let output_format = if publisher.url.starts_with("rtsp://") {
        "rtsp"
    } else if publisher.url.starts_with("rtmp://") {
        "flv"
    } else {
        "mpegts"
    };

    let mut args = vec![
        "-re".into(),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgb24".into(),
        "-s".into(),
        format!("{}x{}", config.width, config.height),
        "-r".into(),
        config.fps.to_string(),
        "-i".into(),
        "pipe:0".into(),
        "-an".into(),
    ];

    if output_format == "rtsp" {
        if let Some(transport) = publisher.rtsp_transport {
            args.push("-rtsp_transport".into());
            args.push(transport.to_string());
        }
    }

    args.extend([
        "-c:v".into(),
        codec_name.into(),
        "-preset".into(),
        "ultrafast".into(),
    ]);

    if matches!(publisher.codec, PublishCodec::H264) {
        args.extend([
            "-tune".into(),
            "zerolatency".into(),
            "-bf".into(),
            "0".into(),
            "-g".into(),
            config.fps.to_string(),
            "-keyint_min".into(),
            config.fps.to_string(),
            "-x264-params".into(),
            format!("keyint={}:min-keyint={}:scenecut=0", config.fps, config.fps),
        ]);
    } else {
        args.extend([
            "-tune".into(),
            "zerolatency".into(),
            "-g".into(),
            "1".into(),
            "-keyint_min".into(),
            "1".into(),
            "-x265-params".into(),
            "keyint=1:min-keyint=1:scenecut=0:bframes=0:repeat-headers=1".into(),
        ]);
    }

    args.extend([
        "-bf".into(),
        "0".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-f".into(),
        output_format.into(),
    ]);

    if output_format == "flv"
        && matches!(publisher.codec, PublishCodec::H265)
        && publisher.rtmp_enhanced_hevc
    {
        args.push("-rtmp_enhanced_codecs".into());
        args.push("hvc1".into());
    }

    args.push("-y".into());
    args.push(publisher.url.clone());
    args
}

fn spawn_frame_writer(
    mut stdin: ChildStdin,
    stop: Arc<AtomicBool>,
    config: RealtimeProbeConfig,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let frame_interval = Duration::from_nanos(1_000_000_000u64 / config.fps as u64);
        let mut seq = 0u32;
        let mut next_frame_at = Instant::now();

        while !stop.load(Ordering::Relaxed) {
            let marker = FrameMarker {
                source_wall_ms: now_ms(),
                seq,
            };
            let frame = generate_timestamp_frame(&config, marker);
            if stdin.write_all(&frame).is_err() {
                break;
            }
            seq = seq.wrapping_add(1);
            next_frame_at += frame_interval;
            let now = Instant::now();
            if next_frame_at > now {
                thread::sleep(next_frame_at - now);
            } else {
                next_frame_at = now;
            }
        }
    })
}

fn generate_timestamp_frame(config: &RealtimeProbeConfig, marker: FrameMarker) -> Vec<u8> {
    let mut frame = vec![0u8; config.width * config.height * 3];

    for y in 0..config.height {
        for x in 0..config.width {
            let offset = (y * config.width + x) * 3;
            frame[offset] = 24;
            frame[offset + 1] = 96;
            frame[offset + 2] = 160;
        }
    }

    let mut payload = [0u8; MARKER_BYTES];
    payload[0..2].copy_from_slice(&MARKER_MAGIC.to_be_bytes());
    payload[2..10].copy_from_slice(&marker.source_wall_ms.to_be_bytes());
    payload[10..14].copy_from_slice(&marker.seq.to_be_bytes());

    for bit_index in 0..MARKER_BITS {
        let byte = payload[bit_index / 8];
        let bit = (byte >> (7 - (bit_index % 8))) & 1;
        paint_marker_block(&mut frame, config.width, bit_index, bit);
    }

    frame
}

fn paint_marker_block(frame: &mut [u8], width: usize, bit_index: usize, bit: u8) {
    let col = bit_index % MARKER_COLS;
    let row = bit_index / MARKER_COLS;
    let x0 = MARKER_X + col * MARKER_BLOCK;
    let y0 = MARKER_Y + row * MARKER_BLOCK;
    let value = if bit == 1 { 255 } else { 0 };

    for y in y0..(y0 + MARKER_BLOCK) {
        for x in x0..(x0 + MARKER_BLOCK) {
            let offset = (y * width + x) * 3;
            frame[offset] = value;
            frame[offset + 1] = value;
            frame[offset + 2] = value;
        }
    }
}

fn decode_marker(frame: &[u8], width: usize, height: usize) -> Option<FrameMarker> {
    let marker_height = MARKER_Y + ((MARKER_BITS + MARKER_COLS - 1) / MARKER_COLS) * MARKER_BLOCK;
    let marker_width = MARKER_X + MARKER_COLS * MARKER_BLOCK;
    if width < marker_width || height < marker_height || frame.len() < width * height * 3 {
        return None;
    }

    let mut payload = [0u8; MARKER_BYTES];
    for bit_index in 0..MARKER_BITS {
        let col = bit_index % MARKER_COLS;
        let row = bit_index / MARKER_COLS;
        let x = MARKER_X + col * MARKER_BLOCK + MARKER_BLOCK / 2;
        let y = MARKER_Y + row * MARKER_BLOCK + MARKER_BLOCK / 2;
        let offset = (y * width + x) * 3;
        let brightness =
            (frame[offset] as u16 + frame[offset + 1] as u16 + frame[offset + 2] as u16) / 3;
        if brightness > 128 {
            payload[bit_index / 8] |= 1 << (7 - (bit_index % 8));
        }
    }

    if u16::from_be_bytes([payload[0], payload[1]]) != MARKER_MAGIC {
        return None;
    }

    let source_wall_ms = u64::from_be_bytes(payload[2..10].try_into().ok()?);
    let seq = u32::from_be_bytes(payload[10..14].try_into().ok()?);
    Some(FrameMarker {
        source_wall_ms,
        seq,
    })
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = if percentile >= 100 {
        sorted.len() - 1
    } else {
        ((sorted.len() - 1) * percentile) / 100
    };
    sorted[index]
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
