mod analysis;
mod core;
mod hls;
mod http;
mod http_flv;
mod record;
mod rtmp;
mod rtsp;
mod snapshot;
mod webrtc;

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::info;
use tracing_appender::{non_blocking, rolling};
use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    fmt,
    prelude::*,
};

use crate::core::{Config, StreamManager, StreamProtocol, StreamSourceMode};
use crate::hls::HlsConfig as HlsModuleConfig;
use crate::record::{RecordFormat, RecorderManager};
use crate::snapshot::SnapshotManager;
static LOG_GUARD: Mutex<Option<tracing_appender::non_blocking::WorkerGuard>> = Mutex::new(None);

fn server_task_result(
    name: &str,
    result: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> Result<()> {
    let inner = result.with_context(|| format!("{name} server task failed"))?;
    inner.with_context(|| format!("{name} server failed"))?;
    Err(anyhow::anyhow!("{name} server stopped unexpectedly"))
}

fn read_config() -> Result<Config> {
    let config_path = Path::new("config.toml");

    if config_path.exists() {
        info!("Reading config from: {}", config_path.display());
        let config_content = fs::read_to_string(config_path)?;
        let config: Config = toml::from_str(&config_content)?;
        Ok(config)
    } else {
        info!("Config file not found, using default config");
        Ok(Config::default())
    }
}

fn parse_log_level(level_str: &str) -> LevelFilter {
    match level_str.to_lowercase().as_str() {
        "trace" => LevelFilter::TRACE,
        "debug" => LevelFilter::DEBUG,
        "info" => LevelFilter::INFO,
        "warn" => LevelFilter::WARN,
        "error" => LevelFilter::ERROR,
        _ => LevelFilter::INFO,
    }
}

fn init_logging(config: &Config) -> Result<()> {
    let default_level = parse_log_level(&config.log.level);

    let mut filter = EnvFilter::builder()
        .with_default_directive(default_level.into())
        .from_env_lossy();

    if let Some(modules) = &config.log.modules {
        for (module, level) in modules {
            let target = if module.contains("::") {
                module.clone()
            } else {
                format!("vcp_media_server::{}", module)
            };
            let level_filter = parse_log_level(level);
            filter = filter.add_directive(format!("{}={}", target, level_filter).parse()?);
            info!("Module log level: {}={}", target, level);
        }
    }

    let log_path = &config.log.path;
    if let Some(parent_dir) = std::path::Path::new(log_path).parent() {
        fs::create_dir_all(parent_dir)?;
    }

    let file_appender = rolling::daily(
        std::path::Path::new(log_path)
            .parent()
            .unwrap_or(std::path::Path::new(".")),
        std::path::Path::new(log_path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap(),
    );

    let (non_blocking, guard) = non_blocking(file_appender);

    *LOG_GUARD.lock().unwrap() = Some(guard);

    let format = time::format_description::parse(
        "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]",
    )
    .unwrap();

    let console_layer = tracing_subscriber::fmt::Layer::new()
        .with_timer(fmt::time::LocalTime::new(format.clone()))
        .with_writer(std::io::stdout)
        .with_level(true)
        .with_target(true)
        .with_filter(filter.clone());

    let file_layer = tracing_subscriber::fmt::Layer::new()
        .with_timer(fmt::time::LocalTime::new(format))
        .with_writer(non_blocking)
        .with_level(true)
        .with_target(true)
        .with_ansi(false)
        .with_filter(filter);

    let subscriber = tracing_subscriber::Registry::default()
        .with(console_layer)
        .with(file_layer);

    tracing::subscriber::set_global_default(subscriber)?;

    info!(
        "Logging initialized: level={}, path={}",
        config.log.level, config.log.path
    );

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = read_config()?;

    init_logging(&config)?;

    info!("Starting Media Server...");

    let stream_manager = Arc::new(StreamManager::new());

    for stream_config in &config.streams {
        let source = stream_config
            .source
            .as_ref()
            .map_or(StreamSourceMode::Push, |s| {
                match s.to_uppercase().as_str() {
                    "PULL" => StreamSourceMode::Pull,
                    "PUSH" => StreamSourceMode::Push,
                    _ => StreamSourceMode::Push,
                }
            });

        let protocol = stream_config
            .protocol
            .as_ref()
            .map_or(StreamProtocol::Unknown, |p| {
                match p.to_uppercase().as_str() {
                    "RTSP" => StreamProtocol::RTSP,
                    "RTMP" => StreamProtocol::RTMP,
                    "WEBRTC" => StreamProtocol::WebRTC,
                    "HTTP" => StreamProtocol::HTTP,
                    _ => StreamProtocol::Unknown,
                }
            });

        let source_clone = source.clone();
        let protocol_clone = protocol.clone();
        stream_manager.create_stream(
            &stream_config.id,
            source,
            protocol,
            stream_config.pull_url.clone(),
        );
        info!(
            "Created stream: {} (source: {:?}, protocol: {:?})",
            stream_config.id, source_clone, protocol_clone
        );
    }

    // Initialize HLS server
    let hls_config = config
        .hls
        .as_ref()
        .map(|h| HlsModuleConfig {
            enabled: h.enabled,
            segment_duration: h.segment_duration.unwrap_or(1.0),
            max_segments: h.max_segments.unwrap_or(1),
            output_dir: h.output_dir.clone().unwrap_or("./hls".to_string()),
        })
        .unwrap_or_default();

    let hls_server = Arc::new(hls::HlsServer::new(
        stream_manager.clone(),
        hls_config.clone(),
    ));
    if hls_config.enabled {
        hls_server.start_idle_reaper();
    }
    let hls_server_publish = if hls_config.enabled {
        Some(hls_server.clone())
    } else {
        None
    };

    let rtsp_server = rtsp::RtspServer::new(
        stream_manager.clone(),
        config.rtsp.port,
        hls_server_publish.clone(),
    );
    let webrtc_server = webrtc::WebrtcServer::new(
        stream_manager.clone(),
        config.webrtc.port,
        hls_server_publish.clone(),
    );

    let hls_server_http = if hls_config.enabled {
        Some(hls_server.clone())
    } else {
        None
    };
    let hls_server_rtmp = hls_server_publish.clone();

    // Initialize HTTP-FLV server
    let http_flv_enabled = config.http_flv.as_ref().map(|c| c.enabled).unwrap_or(true);
    let http_flv_server = Arc::new(http_flv::HttpFlvServer::new(stream_manager.clone()));
    let http_flv_server_http = if http_flv_enabled {
        Some(http_flv_server.clone())
    } else {
        None
    };

    let record_config = config
        .record
        .as_ref()
        .map(|c| record::RecordConfig {
            enabled: c.enabled,
            base_dir: c
                .base_dir
                .clone()
                .map(Into::into)
                .unwrap_or_else(|| "./recordings".into()),
            default_format: c
                .default_format
                .as_deref()
                .and_then(RecordFormat::parse)
                .unwrap_or(RecordFormat::Ts),
            segment_duration: std::time::Duration::from_secs(
                c.segment_duration_sec.unwrap_or(300).max(1),
            ),
            align_keyframe: c.align_keyframe.unwrap_or(true),
        })
        .unwrap_or_default();
    let recorder_manager = Arc::new(RecorderManager::new(
        stream_manager.clone(),
        record_config.clone(),
    ));
    let recorder_http = if record_config.enabled {
        Some(recorder_manager.clone())
    } else {
        None
    };

    let analysis_config = config
        .analysis
        .as_ref()
        .map(|c| analysis::AnalysisConfig {
            enabled: c.enabled,
            default_sample_interval: c.default_sample_interval.unwrap_or(1).max(1),
            max_events_per_stream: c.max_events_per_stream.unwrap_or(256).max(1),
        })
        .unwrap_or_default();
    let analysis_manager = Arc::new(analysis::AnalysisManager::new(
        stream_manager.clone(),
        analysis_config.clone(),
    ));
    let analysis_http = if analysis_config.enabled {
        Some(analysis_manager.clone())
    } else {
        None
    };

    let snapshot_config = config
        .snapshot
        .as_ref()
        .map(|c| snapshot::SnapshotConfig {
            enabled: c.enabled,
            base_dir: c
                .base_dir
                .clone()
                .map(Into::into)
                .unwrap_or_else(|| "./snapshots".into()),
            ffmpeg_path: c
                .ffmpeg_path
                .clone()
                .unwrap_or_else(|| "ffmpeg".to_string()),
            wait_keyframe: std::time::Duration::from_millis(
                c.wait_keyframe_ms.unwrap_or(1_000).max(1),
            ),
        })
        .unwrap_or_default();
    let snapshot_manager = Arc::new(SnapshotManager::new(
        stream_manager.clone(),
        snapshot_config.clone(),
    ));
    let snapshot_http = if snapshot_config.enabled {
        Some(snapshot_manager.clone())
    } else {
        None
    };

    let http_server = http::HttpServer::new(
        stream_manager.clone(),
        config.http.port,
        hls_server_http,
        http_flv_server_http,
        recorder_http,
        analysis_http,
        snapshot_http,
    );

    let rtmp_server =
        rtmp::RtmpServer::new(stream_manager.clone(), config.rtmp.port, hls_server_rtmp);

    let rtmp_handle = tokio::spawn(async move { rtmp_server.start().await });

    let rtsp_handle = tokio::spawn(async move { rtsp_server.start().await });

    let webrtc_handle = tokio::spawn(async move { webrtc_server.start().await });

    let http_handle = tokio::spawn(async move { http_server.start().await });

    info!("Media Server started successfully!");
    info!("  RTSP:  rtsp://localhost:{}", config.rtsp.port);
    info!("  RTMP:  rtmp://localhost:{}", config.rtmp.port);
    info!("  HTTP:  http://localhost:{}", config.http.port);
    info!(
        "  HLS:   http://localhost:{}/hls/<stream_id>/live.m3u8",
        config.http.port
    );
    if record_config.enabled {
        info!(
            "  Record API:   http://localhost:{}/api/record/start",
            config.http.port
        );
    }
    if analysis_config.enabled {
        info!(
            "  Analysis API: http://localhost:{}/api/analysis/start",
            config.http.port
        );
    }
    if snapshot_config.enabled {
        info!(
            "  Snapshot API: http://localhost:{}/api/snapshot",
            config.http.port
        );
    }
    info!(
        "  FLV:   http://localhost:{}/flv/<stream_id>",
        config.http.port
    );
    info!("  WebRTC: ws://localhost:{}", config.webrtc.port);

    let result = tokio::select! {
        res = rtmp_handle => server_task_result("RTMP", res),
        res = rtsp_handle => server_task_result("RTSP", res),
        res = webrtc_handle => server_task_result("WebRTC", res),
        res = http_handle => server_task_result("HTTP", res),
    };

    if let Err(e) = &result {
        tracing::error!("Media Server stopped: {e:#}");
    }

    result
}
