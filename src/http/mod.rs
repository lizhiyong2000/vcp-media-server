use anyhow::Result;
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, Duration, Instant};
use tracing::{error, info, warn};

use crate::core::dispatch::DispatchError;
use crate::core::{
    CodecType, DispatchPolicy, DispatchReader, StreamManager, StreamProtocol, StreamSourceMode,
    Track,
};
use crate::hls::HlsServer;
use crate::http_flv::{format_chunk, HttpFlvServer, HttpFlvSession};
use crate::rtmp::RtmpPuller;
use crate::rtsp::{RtspPuller, RtspPusher};
use crate::webrtc::request_publisher_keyframe;

pub struct HttpServer {
    stream_manager: Arc<StreamManager>,
    port: u16,
    hls_server: Option<Arc<HlsServer>>,
    http_flv_server: Option<Arc<HttpFlvServer>>,
}

impl HttpServer {
    pub fn new(
        stream_manager: Arc<StreamManager>,
        port: u16,
        hls_server: Option<Arc<HlsServer>>,
        http_flv_server: Option<Arc<HttpFlvServer>>,
    ) -> Self {
        Self {
            stream_manager,
            port,
            hls_server,
            http_flv_server,
        }
    }

    pub async fn start(&self) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.port);
        info!("[HTTP] Initializing HTTP API server on {}", addr);

        let listener = TcpListener::bind(&addr).await?;
        info!("[HTTP] HTTP API server is ready and listening");
        info!("[HTTP] API endpoints:");
        info!("[HTTP]   GET  /                  - Server info");
        info!("[HTTP]   GET  /health             - Health check");
        info!("[HTTP]   GET  /api/streams        - List all streams");
        info!("[HTTP]   GET  /api/stream/<id>    - Get stream info");
        info!("[HTTP]   POST /api/streams        - Create new stream");
        info!("[HTTP]   DELETE /api/stream/<id>  - Delete stream");
        info!("[HTTP]   POST /api/rtsp/pull      - RTSP pull from remote URL");
        info!("[HTTP]   POST /api/rtsp/push      - RTSP push to remote URL");
        info!("[HTTP]   POST /api/rtmp/pull      - RTMP pull from remote URL");
        info!("[HTTP]   GET  /webrtc/webrtc-test.html - WebRTC test page");
        if self.hls_server.is_some() {
            info!("[HTTP]   GET  /hls/<stream_id>/live.m3u8 - HLS playlist");
            info!("[HTTP]   GET  /hls/<stream_id>/<segment>.ts - HLS segment");
        }
        if self.http_flv_server.is_some() {
            info!("[HTTP]   GET  /flv/<stream_id>  - HTTP-FLV live stream");
        }

        loop {
            match listener.accept().await {
                Ok((socket, peer_addr)) => {
                    info!("[HTTP] New request from {}", peer_addr);
                    let manager = self.stream_manager.clone();
                    let hls = self.hls_server.clone();
                    let flv = self.http_flv_server.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_connection(socket, manager, hls, flv).await {
                            error!("[HTTP] Connection error from {}: {}", peer_addr, e);
                        }
                    });
                }
                Err(e) => {
                    error!("[HTTP] Failed to accept connection: {}", e);
                }
            }
        }
    }

    async fn handle_connection(
        socket: TcpStream,
        manager: Arc<StreamManager>,
        hls_server: Option<Arc<HlsServer>>,
        flv_server: Option<Arc<HttpFlvServer>>,
    ) -> Result<()> {
        let mut buffer = vec![0u8; 8192];
        let mut socket = socket;

        let n = socket.read(&mut buffer).await?;
        if n == 0 {
            return Ok(());
        }

        let request = String::from_utf8_lossy(&buffer[..n]).to_string();

        // Check for HLS or FLV streaming requests first
        let first_line = request.lines().next().unwrap_or("");
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() >= 2 {
            let path = parts[1];

            // HLS playlist request
            if path.starts_with("/hls/") && path.ends_with(".m3u8") {
                if let Some(ref hls) = hls_server {
                    let stream_id = path
                        .trim_start_matches("/hls/")
                        .trim_end_matches("/live.m3u8");
                    if manager.get_stream(&stream_id.to_string()).is_none() {
                        let response = Self::http_response(404, "Not Found", "Stream not found");
                        socket.write_all(response.as_bytes()).await?;
                        socket.flush().await?;
                        return Ok(());
                    }
                    manager.ensure_stream_broadcast(stream_id);
                    let _ = hls.ensure_stream(stream_id, false).await;
                    request_publisher_keyframe(stream_id);

                    let deadline = Instant::now() + Duration::from_secs(3);
                    let mut playlist = hls.get_playlist(stream_id);
                    while playlist.is_none() && Instant::now() < deadline {
                        request_publisher_keyframe(stream_id);
                        sleep(Duration::from_millis(50)).await;
                        playlist = hls.get_playlist(stream_id);
                    }
                    let playlist = playlist.unwrap_or_else(|| hls.empty_playlist());
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache, no-store, must-revalidate\r\nConnection: close\r\n\r\n{}",
                        playlist.len(), playlist
                    );
                    socket.write_all(response.as_bytes()).await?;
                    socket.shutdown().await?;
                    return Ok(());
                }
                let response = Self::http_response(404, "Not Found", "");
                socket.write_all(response.as_bytes()).await?;
                socket.flush().await?;
                return Ok(());
            }

            // HLS segment request
            if path.starts_with("/hls/") && path.ends_with(".ts") {
                if let Some(ref hls) = hls_server {
                    let path_parts: Vec<&str> =
                        path.trim_start_matches("/hls/").split('/').collect();
                    if path_parts.len() >= 2 {
                        let stream_id = path_parts[0];
                        let filename = path_parts[1];
                        if let Some(seg_path) = hls.get_segment_path(stream_id, filename) {
                            if let Ok(data) = tokio::fs::read(&seg_path).await {
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache, no-store\r\nConnection: close\r\n\r\n",
                                    data.len()
                                );
                                socket.write_all(response.as_bytes()).await?;
                                socket.write_all(&data).await?;
                                socket.shutdown().await?;
                                return Ok(());
                            }
                        }
                    }
                }
                let response = Self::http_response(404, "Not Found", "");
                socket.write_all(response.as_bytes()).await?;
                socket.flush().await?;
                return Ok(());
            }

            // WebRTC test page
            if path == "/webrtc/webrtc-test.html" || path == "/webrtc/" {
                const WEBRTC_TEST_HTML: &str = include_str!("../../webrtc/webrtc-test.html");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\r\n{}",
                    WEBRTC_TEST_HTML.len(),
                    WEBRTC_TEST_HTML
                );
                socket.write_all(response.as_bytes()).await?;
                socket.flush().await?;
                return Ok(());
            }

            // HTTP-FLV request
            if path.starts_with("/flv/") {
                if let Some(ref flv) = flv_server {
                    let stream_id = path.trim_start_matches("/flv/").trim_end_matches('/');
                    if let Some((mut session, mut stream)) = flv.create_session(stream_id) {
                        let stream_id_owned = stream_id.to_string();
                        let manager = flv.stream_manager();
                        manager.ensure_stream_hub(stream_id);
                        let mut reader = match manager
                            .dispatch_subscribe(stream_id, DispatchPolicy::LiveCoalesce)
                        {
                            Some(r) => r,
                            None => {
                                let response =
                                    Self::http_response(404, "Not Found", "Stream not found");
                                socket.write_all(response.as_bytes()).await?;
                                socket.flush().await?;
                                return Ok(());
                            }
                        };

                        // Wait for SPS/PPS before responding so players can probe codecs
                        let deadline = Instant::now() + Duration::from_secs(5);
                        while stream.sps.is_none() || stream.pps.is_none() {
                            if Instant::now() >= deadline {
                                let response = Self::http_response(
                                    503,
                                    "Service Unavailable",
                                    "Stream not ready (waiting for video sequence header)",
                                );
                                socket.write_all(response.as_bytes()).await?;
                                socket.flush().await?;
                                return Ok(());
                            }
                            sleep(Duration::from_millis(50)).await;
                            if let Some(s) = manager.get_stream(&stream_id_owned) {
                                stream = s;
                            }
                        }

                        let http_headers = HttpFlvSession::generate_http_headers();
                        socket.write_all(http_headers.as_bytes()).await?;

                        let initial_data = session.generate_initial_data(&stream);
                        if !initial_data.is_empty() {
                            let chunk = format_chunk(&initial_data);
                            socket.write_all(&chunk).await?;
                        }

                        let mut pending_idr = crate::rtsp::play_egress::prime_rtsp_play(
                            &mut reader,
                            manager,
                            &stream_id_owned,
                        )
                        .await;
                        let mut video_streaming = false;

                        loop {
                            let frames = if let Some(frame) = pending_idr.take() {
                                vec![frame]
                            } else {
                                match reader.recv_batch().await {
                                    Ok(frames) if !frames.is_empty() => frames,
                                    Ok(_) => continue,
                                    Err(DispatchError::Closed) => break,
                                }
                            };
                            for frame in frames {
                                if frame.codec == CodecType::Opus || frame.codec == CodecType::G711
                                {
                                    continue;
                                }
                                if matches!(frame.codec, CodecType::H264 | CodecType::H265)
                                    && !video_streaming
                                {
                                    let is_idr = frame.is_keyframe
                                        || crate::webrtc::h264_util::is_keyframe_annex_b(
                                            &frame.data,
                                        );
                                    if !is_idr {
                                        continue;
                                    }
                                    video_streaming = true;
                                }
                                if frame.codec == CodecType::AAC && frame.data.len() < 8 {
                                    continue;
                                }

                                if session.needs_sequence_headers() {
                                    if let Some(stream) =
                                        flv.stream_manager().get_stream(&stream_id_owned)
                                    {
                                        let more = session.generate_initial_data(&stream);
                                        if !more.is_empty() {
                                            let chunk = format_chunk(&more);
                                            if socket.write_all(&chunk).await.is_err() {
                                                break;
                                            }
                                        }
                                    }
                                    if session.needs_sequence_headers() {
                                        continue;
                                    }
                                }

                                let flv_data = session.frame_to_flv(&frame);
                                if !flv_data.is_empty() {
                                    let chunk = format_chunk(&flv_data);
                                    if socket.write_all(&chunk).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        socket.flush().await?;
                        return Ok(());
                    }
                }
                let response = Self::http_response(404, "Not Found", "");
                socket.write_all(response.as_bytes()).await?;
                socket.flush().await?;
                return Ok(());
            }
        }

        // Regular API request
        let response = Self::process_request(&request, manager.clone()).await?;
        socket.write_all(response.as_bytes()).await?;
        socket.flush().await?;

        Ok(())
    }

    async fn process_request(request: &str, manager: Arc<StreamManager>) -> Result<String> {
        let lines: Vec<&str> = request.lines().collect();
        if lines.is_empty() {
            return Ok(Self::http_response(400, "Bad Request", ""));
        }

        let first_line = lines[0];
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() < 2 {
            return Ok(Self::http_response(400, "Bad Request", ""));
        }

        let method = parts[0];
        let path = parts[1];

        info!("HTTP {} {}", method, path);

        match (method, path) {
            ("GET", "/api/streams") => {
                let stream_ids = manager.list_streams();
                let streams: Vec<serde_json::Value> = stream_ids.iter().map(|id| {
                    if let Some(stream) = manager.get_stream(id) {
                        json!({
                            "id": stream.id,
                            "status": stream.status.as_str(),
                            "status_description": stream.status.description(),
                            "playback_status": stream.playback_status.as_str(),
                            "playback_description": stream.playback_status.description(),
                            "source": format!("{:?}", stream.source),
                            "protocol": format!("{:?}", stream.protocol),
                            "pull_url": stream.pull_url,
                            "tracks": stream.tracks.len()
                        })
                    } else {
                        json!({"id": id, "status": "unknown", "status_description": "Stream not found", "playback_status": "unknown", "playback_description": "Stream not found", "source": "unknown", "protocol": "unknown", "pull_url": null, "tracks": 0})
                    }
                }).collect();

                let body = json!({
                    "streams": streams,
                    "count": streams.len()
                })
                .to_string();
                Ok(Self::http_response(200, "OK", &body))
            }
            ("GET", "/api/stream") | ("GET", "/api/stream/") => {
                let body = json!({
                    "usage": "GET /api/stream/<stream_id>"
                })
                .to_string();
                Ok(Self::http_response(200, "OK", &body))
            }
            ("GET", path) if path.starts_with("/api/stream/") => {
                let stream_id = path.trim_start_matches("/api/stream/");
                if let Some(stream) = manager.get_stream(&stream_id.to_string()) {
                    let tracks: Vec<serde_json::Value> = stream
                        .tracks
                        .iter()
                        .map(|t| {
                            json!({
                                "id": t.id,
                                "codec": format!("{:?}", t.codec),
                                "payload_type": t.payload_type,
                                "clock_rate": t.clock_rate
                            })
                        })
                        .collect();

                    let body = json!({
                        "id": stream.id,
                        "status": format!("{:?}", stream.status),
                        "source": format!("{:?}", stream.source),
                        "protocol": format!("{:?}", stream.protocol),
                        "pull_url": stream.pull_url,
                        "tracks": tracks
                    })
                    .to_string();
                    Ok(Self::http_response(200, "OK", &body))
                } else {
                    Ok(Self::http_response(404, "Not Found", ""))
                }
            }
            ("POST", "/api/streams") => {
                // Parse request body for stream configuration
                let body_start = request
                    .find("\r\n\r\n")
                    .map(|i| &request[i + 4..])
                    .unwrap_or("");
                let result = serde_json::from_str::<serde_json::Value>(body_start)
                    .or_else(|_| serde_json::from_str::<serde_json::Value>(&request));

                let (stream_id, source, protocol, pull_url) = if let Ok(json) = result {
                    let id = json
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("live")
                        .to_string();

                    let source = json.get("source").and_then(|v| v.as_str()).map_or(
                        StreamSourceMode::Push,
                        |s| match s.to_uppercase().as_str() {
                            "PULL" => StreamSourceMode::Pull,
                            "PUSH" => StreamSourceMode::Push,
                            _ => StreamSourceMode::Push,
                        },
                    );

                    let protocol = json.get("protocol").and_then(|v| v.as_str()).map_or(
                        StreamProtocol::Unknown,
                        |p| match p.to_uppercase().as_str() {
                            "RTSP" => StreamProtocol::RTSP,
                            "RTMP" => StreamProtocol::RTMP,
                            "WEBRTC" => StreamProtocol::WebRTC,
                            "HTTP" => StreamProtocol::HTTP,
                            _ => StreamProtocol::Unknown,
                        },
                    );

                    let pull_url = json
                        .get("pull_url")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    (id, source, protocol, pull_url)
                } else {
                    (
                        "live".to_string(),
                        StreamSourceMode::Push,
                        StreamProtocol::Unknown,
                        None,
                    )
                };

                let stream = manager.create_stream(&stream_id, source, protocol, pull_url);

                let body = json!({
                    "id": stream.id,
                    "status": format!("{:?}", stream.status),
                    "source": format!("{:?}", stream.source),
                    "protocol": format!("{:?}", stream.protocol),
                    "pull_url": stream.pull_url,
                    "message": "Stream created"
                })
                .to_string();
                Ok(Self::http_response(201, "Created", &body))
            }
            ("POST", "/api/rtsp/pull") => {
                let body_start = request
                    .find("\r\n\r\n")
                    .map(|i| &request[i + 4..])
                    .unwrap_or("");
                let parse_result = serde_json::from_str::<serde_json::Value>(body_start);

                let (remote_url, local_stream_id) = if let Ok(json) = &parse_result {
                    let url = json.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let stream_id = json
                        .get("stream_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("pulled_stream");
                    (url.to_string(), stream_id.to_string())
                } else {
                    return Ok(Self::http_response(
                        400,
                        "Bad Request",
                        "{\"error\":\"Invalid JSON body\"}",
                    ));
                };

                if remote_url.is_empty() {
                    return Ok(Self::http_response(
                        400,
                        "Bad Request",
                        "{\"error\":\"Missing 'url' parameter\"}",
                    ));
                }

                info!(
                    "[HTTP] Starting RTSP pull from {} to stream {}",
                    remote_url, local_stream_id
                );

                let manager_clone = manager.clone();
                let remote_url_clone = remote_url.clone();
                let local_stream_id_clone = local_stream_id.clone();

                tokio::spawn(async move {
                    let puller = RtspPuller::new(manager_clone);
                    if let Err(e) = puller.pull(&remote_url_clone, &local_stream_id_clone).await {
                        error!("[RTSP Puller] Failed to pull stream: {}", e);
                    }
                });

                let body = json!({
                    "stream_id": local_stream_id,
                    "remote_url": remote_url,
                    "message": "RTSP pull started"
                })
                .to_string();
                Ok(Self::http_response(200, "OK", &body))
            }
            ("POST", "/api/rtmp/pull") => {
                let body_start = request
                    .find("\r\n\r\n")
                    .map(|i| &request[i + 4..])
                    .unwrap_or("");
                let parse_result = serde_json::from_str::<serde_json::Value>(body_start);

                let (remote_url, local_stream_id) = if let Ok(json) = &parse_result {
                    let url = json.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let stream_id = json
                        .get("stream_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("rtmp_pulled");
                    (url.to_string(), stream_id.to_string())
                } else {
                    return Ok(Self::http_response(
                        400,
                        "Bad Request",
                        "{\"error\":\"Invalid JSON body\"}",
                    ));
                };

                if remote_url.is_empty() {
                    return Ok(Self::http_response(
                        400,
                        "Bad Request",
                        "{\"error\":\"Missing 'url' parameter\"}",
                    ));
                }

                info!(
                    "[HTTP] Starting RTMP pull from {} to stream {}",
                    remote_url, local_stream_id
                );

                let manager_clone = manager.clone();
                let remote_url_clone = remote_url.clone();
                let local_stream_id_clone = local_stream_id.clone();

                tokio::spawn(async move {
                    let puller = RtmpPuller::new(manager_clone);
                    if let Err(e) = puller.pull(&remote_url_clone, &local_stream_id_clone).await {
                        error!("[RTMP Puller] Failed to pull stream: {}", e);
                    }
                });

                let body = json!({
                    "stream_id": local_stream_id,
                    "remote_url": remote_url,
                    "play_url": format!("rtmp://127.0.0.1:1935/live/{}", local_stream_id),
                    "message": "RTMP pull started"
                })
                .to_string();
                Ok(Self::http_response(200, "OK", &body))
            }
            ("POST", "/api/rtsp/push") => {
                let body_start = request
                    .find("\r\n\r\n")
                    .map(|i| &request[i + 4..])
                    .unwrap_or("");
                let parse_result = serde_json::from_str::<serde_json::Value>(body_start);

                let (stream_id, remote_url) = if let Ok(json) = &parse_result {
                    let id = json.get("stream_id").and_then(|v| v.as_str()).unwrap_or("");
                    let url = json.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    (id.to_string(), url.to_string())
                } else {
                    return Ok(Self::http_response(
                        400,
                        "Bad Request",
                        "{\"error\":\"Invalid JSON body\"}",
                    ));
                };

                if stream_id.is_empty() {
                    return Ok(Self::http_response(
                        400,
                        "Bad Request",
                        "{\"error\":\"Missing 'stream_id' parameter\"}",
                    ));
                }

                if remote_url.is_empty() {
                    return Ok(Self::http_response(
                        400,
                        "Bad Request",
                        "{\"error\":\"Missing 'url' parameter\"}",
                    ));
                }

                info!(
                    "[HTTP] Starting RTSP push from stream {} to {}",
                    stream_id, remote_url
                );

                let stream = match manager.get_stream(&stream_id) {
                    Some(s) => s,
                    None => {
                        return Ok(Self::http_response(
                            404,
                            "Not Found",
                            "{\"error\":\"Stream not found\"}",
                        ));
                    }
                };

                let tracks: Vec<Track> = stream.tracks.iter().cloned().collect();
                if tracks.is_empty() {
                    return Ok(Self::http_response(
                        400,
                        "Bad Request",
                        "{\"error\":\"Stream has no tracks\"}",
                    ));
                }

                let manager_clone = manager.clone();
                let stream_id_clone = stream_id.clone();
                let remote_url_clone = remote_url.clone();

                tokio::spawn(async move {
                    let mut pusher =
                        RtspPusher::new(manager_clone, &remote_url_clone, &stream_id_clone);
                    pusher.set_tracks(tracks);
                    if let Err(e) = pusher.start().await {
                        error!("[RTSP Pusher] Failed to push stream: {}", e);
                    }
                });

                let body = json!({
                    "stream_id": stream_id,
                    "remote_url": remote_url,
                    "message": "RTSP push started"
                })
                .to_string();
                Ok(Self::http_response(200, "OK", &body))
            }
            ("DELETE", path) if path.starts_with("/api/stream/") => {
                let stream_id = path.trim_start_matches("/api/stream/");
                if manager.remove_stream(&stream_id.to_string()).is_some() {
                    Ok(Self::http_response(200, "OK", ""))
                } else {
                    Ok(Self::http_response(404, "Not Found", ""))
                }
            }
            ("GET", "/health") => Ok(Self::http_response(200, "OK", "{\"status\":\"healthy\"}")),
            ("GET", "/") => {
                let mut endpoints = serde_json::Map::new();
                endpoints.insert("GET /api/streams".to_string(), json!("List all streams"));
                endpoints.insert("GET /api/stream/<id>".to_string(), json!("Get stream info"));
                endpoints.insert(
                    "POST /api/streams".to_string(),
                    json!("Create a new stream"),
                );
                endpoints.insert(
                    "DELETE /api/stream/<id>".to_string(),
                    json!("Delete a stream"),
                );
                endpoints.insert(
                    "POST /api/rtsp/pull".to_string(),
                    json!("Start RTSP pull from remote URL"),
                );
                endpoints.insert(
                    "POST /api/rtmp/pull".to_string(),
                    json!("Start RTMP pull from remote URL"),
                );
                endpoints.insert(
                    "POST /api/rtsp/push".to_string(),
                    json!("Start RTSP push to remote URL"),
                );
                endpoints.insert(
                    "GET /hls/<stream_id>/live.m3u8".to_string(),
                    json!("HLS playlist"),
                );
                endpoints.insert(
                    "GET /hls/<stream_id>/<segment>.ts".to_string(),
                    json!("HLS segment"),
                );
                endpoints.insert(
                    "GET /flv/<stream_id>".to_string(),
                    json!("HTTP-FLV live stream"),
                );
                endpoints.insert("GET /health".to_string(), json!("Health check"));

                let body = json!({
                    "name": "Media Server",
                    "version": "0.1.0",
                    "endpoints": endpoints,
                    "protocols": {
                        "RTMP": "rtmp://localhost:1935",
                        "RTSP": "rtsp://localhost:554",
                        "HLS": "http://localhost:8081/hls/<stream_id>/live.m3u8",
                        "HTTP-FLV": "http://localhost:8081/flv/<stream_id>",
                        "WebRTC": "ws://localhost:9080"
                    }
                })
                .to_string();
                Ok(Self::http_response(200, "OK", &body))
            }
            _ => Ok(Self::http_response(404, "Not Found", "")),
        }
    }

    fn http_response(code: u32, reason: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {} {}\r\n\
            Content-Type: application/json\r\n\
            Content-Length: {}\r\n\
            Connection: close\r\n\
            Access-Control-Allow-Origin: *\r\n\
            \r\n\
            {}",
            code,
            reason,
            body.len(),
            body
        )
    }
}
