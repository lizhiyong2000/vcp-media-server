/// RTMP client puller: connect to remote RTMP server, play a stream,
/// and relay A/V frames into StreamManager for local RTMP playback.
use anyhow::{anyhow, Result};
use bytes::{Bytes, BytesMut};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, error, info, warn};

use super::amf0::{self, Amf0Value};
use super::chunk::{self, ChunkAssembler, ChunkHeader};
use crate::core::{
    CodecType, MediaFrame, StreamManager, StreamProtocol, StreamSourceMode, MILLISECOND_CLOCK_RATE,
};

#[derive(Debug, Clone)]
pub struct RtmpUrl {
    pub host: String,
    pub port: u16,
    pub app: String,
    pub stream_name: String,
    pub tc_url: String,
}

pub fn parse_rtmp_url(url: &str) -> Result<RtmpUrl> {
    let parsed = url::Url::parse(url).map_err(|e| anyhow!("Invalid RTMP URL: {}", e))?;
    if parsed.scheme() != "rtmp" {
        return Err(anyhow!("URL scheme must be rtmp, got {}", parsed.scheme()));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("Missing host in RTMP URL"))?
        .to_string();
    let port = parsed.port().unwrap_or(1935);

    let path = parsed.path().trim_start_matches('/');
    let mut parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return Err(anyhow!(
            "RTMP URL path must be /<app>/<stream_name>, got /{}",
            path
        ));
    }

    let stream_name = parts.pop().unwrap().to_string();
    let app = parts.join("/");
    let tc_url = format!("rtmp://{}:{}/{}", host, port, app);

    Ok(RtmpUrl {
        host,
        port,
        app,
        stream_name,
        tc_url,
    })
}

pub struct RtmpPuller {
    stream_manager: Arc<StreamManager>,
}

impl RtmpPuller {
    pub fn new(stream_manager: Arc<StreamManager>) -> Self {
        Self { stream_manager }
    }

    pub async fn pull(&self, remote_url: &str, local_stream_id: &str) -> Result<()> {
        let rtmp_url = parse_rtmp_url(remote_url)?;

        info!("[RTMP Puller] =========================================");
        info!(
            "[RTMP Puller] Pull {} (app={}, stream={}) -> local stream '{}'",
            remote_url, rtmp_url.app, rtmp_url.stream_name, local_stream_id
        );
        info!("[RTMP Puller] =========================================");

        let addr = format!("{}:{}", rtmp_url.host, rtmp_url.port);
        let mut stream = TcpStream::connect(&addr).await?;
        info!("[RTMP Puller] Connected to {}", addr);

        client_handshake(&mut stream).await?;
        info!("[RTMP Puller] Handshake completed");

        let (mut read_half, mut write_half) = stream.into_split();

        let mut buf = BytesMut::with_capacity(8192);
        let mut chunk_states: HashMap<u32, ChunkAssembler> = HashMap::new();
        let mut chunk_message_headers: HashMap<u32, ChunkHeader> = HashMap::new();
        let mut chunk_size: usize = 128;
        let mut tx_id = 1.0_f64;
        let mut server_stream_id: u32 = 1;

        // Step 1: connect
        let connect_msg = build_connect_command(tx_id, &rtmp_url.app, &rtmp_url.tc_url);
        write_half.write_all(&connect_msg).await?;
        write_half.flush().await?;
        info!("[RTMP Puller] Sent connect (tx={})", tx_id);

        wait_for_result(
            &mut read_half,
            &mut buf,
            &mut chunk_states,
            &mut chunk_message_headers,
            &mut chunk_size,
            tx_id,
            "connect",
        )
        .await?;
        tx_id += 1.0;

        // Step 2: createStream
        let create_msg = build_create_stream_command(tx_id);
        write_half.write_all(&create_msg).await?;
        write_half.flush().await?;
        info!("[RTMP Puller] Sent createStream (tx={})", tx_id);

        server_stream_id = wait_for_create_stream(
            &mut read_half,
            &mut buf,
            &mut chunk_states,
            &mut chunk_message_headers,
            &mut chunk_size,
            tx_id,
        )
        .await? as u32;
        tx_id += 1.0;

        // Register local stream before play so relay is ready
        self.stream_manager.create_stream(
            local_stream_id,
            StreamSourceMode::Pull,
            StreamProtocol::RTMP,
            Some(remote_url.to_string()),
        );
        let _ = self.stream_manager.set_unpublished(local_stream_id);
        self.stream_manager.set_stream_broadcast(local_stream_id);
        let _ = self.stream_manager.set_publishing(local_stream_id);

        // Step 3: play
        let play_msg = build_play_command(tx_id, &rtmp_url.stream_name, server_stream_id);
        write_half.write_all(&play_msg).await?;
        write_half.flush().await?;
        info!(
            "[RTMP Puller] Sent play stream='{}' msg_stream_id={} (tx={})",
            rtmp_url.stream_name, server_stream_id, tx_id
        );

        wait_for_play_start(
            &mut read_half,
            &mut buf,
            &mut chunk_states,
            &mut chunk_message_headers,
            &mut chunk_size,
            &self.stream_manager,
            local_stream_id,
        )
        .await?;

        info!(
            "[RTMP Puller] Play started, relaying to local stream '{}'",
            local_stream_id
        );
        info!(
            "[RTMP Puller] Local playback: rtmp://127.0.0.1:1935/{}/{}",
            rtmp_url.app, local_stream_id
        );

        let manager = self.stream_manager.clone();
        let local_id = local_stream_id.to_string();
        let mut frames_received: u64 = 0;

        loop {
            let mut read_buf = [0u8; 8192];
            let len = match read_half.read(&mut read_buf).await {
                Ok(0) => {
                    info!("[RTMP Puller] Remote connection closed");
                    break;
                }
                Ok(n) => n,
                Err(e) => {
                    error!("[RTMP Puller] Read error: {}", e);
                    break;
                }
            };

            buf.extend_from_slice(&read_buf[..len]);

            while let Some(msg) = chunk::parse_chunks(
                &mut buf,
                &mut chunk_states,
                &mut chunk_message_headers,
                chunk_size,
            ) {
                if let Some(new_size) = handle_control_message(&msg.payload, msg.msg_type) {
                    chunk_size = new_size;
                }

                if let Some(frame) = rtmp_message_to_frame(&msg, &local_id) {
                    frames_received += 1;
                    if frames_received == 1 {
                        info!(
                            "[RTMP Puller] First relayed frame: codec={:?} keyframe={} ts={} size={}",
                            frame.codec, frame.is_keyframe, frame.timestamp, frame.data.len()
                        );
                    } else if frames_received % 100 == 0 {
                        info!(
                            "[RTMP Puller] Relayed {} frames (codec={:?} ts={})",
                            frames_received, frame.codec, frame.timestamp
                        );
                    }
                    manager.publish_frame(frame);
                } else if msg.msg_type == 0x09 {
                    extract_and_store_sps_pps(&msg.payload, &manager, &local_id);
                }
            }
        }

        let _ = self.stream_manager.set_stopped(local_stream_id);
        info!(
            "[RTMP Puller] Pull ended for stream '{}', total frames={}",
            local_stream_id, frames_received
        );
        Ok(())
    }
}

async fn client_handshake(stream: &mut TcpStream) -> Result<()> {
    let mut c0c1 = vec![0x03u8];
    c0c1.extend(vec![0u8; 1536]);
    stream.write_all(&c0c1).await?;

    let mut response = vec![0u8; 3073];
    stream.read_exact(&mut response).await?;

    if response[0] != 0x03 {
        return Err(anyhow!("Unexpected RTMP version: 0x{:02X}", response[0]));
    }

    stream.write_all(&response[1..1537]).await?;
    Ok(())
}

const CLIENT_CHUNK_SIZE: usize = 128;

fn build_connect_command(tx_id: f64, app: &str, tc_url: &str) -> Vec<u8> {
    let mut props = HashMap::new();
    props.insert("app".to_string(), Amf0Value::String(app.to_string()));
    props.insert(
        "flashVer".to_string(),
        Amf0Value::String("FMLE/3.0 (compatible; Lavf58.2.100)".to_string()),
    );
    props.insert("tcUrl".to_string(), Amf0Value::String(tc_url.to_string()));
    props.insert("fpad".to_string(), Amf0Value::Boolean(false));
    props.insert("capabilities".to_string(), Amf0Value::Number(15.0));
    props.insert("audioCodecs".to_string(), Amf0Value::Number(4071.0));
    props.insert("videoCodecs".to_string(), Amf0Value::Number(252.0));
    props.insert("videoFunction".to_string(), Amf0Value::Number(1.0));

    let payload = amf0::encode(&[
        Amf0Value::String("connect".to_string()),
        Amf0Value::Number(tx_id),
        Amf0Value::Object(props),
    ]);
    chunk::encode_message(0x14, 0, 0, &payload, CLIENT_CHUNK_SIZE, 3)
}

fn build_create_stream_command(tx_id: f64) -> Vec<u8> {
    let payload = amf0::encode(&[
        Amf0Value::String("createStream".to_string()),
        Amf0Value::Number(tx_id),
        Amf0Value::Null,
    ]);
    chunk::encode_message(0x14, 0, 0, &payload, CLIENT_CHUNK_SIZE, 3)
}

fn build_play_command(tx_id: f64, stream_name: &str, msg_stream_id: u32) -> Vec<u8> {
    let payload = amf0::encode(&[
        Amf0Value::String("play".to_string()),
        Amf0Value::Number(tx_id),
        Amf0Value::Null,
        Amf0Value::String(stream_name.to_string()),
        Amf0Value::Number(-2000.0),
    ]);
    chunk::encode_message(0x14, 0, msg_stream_id, &payload, CLIENT_CHUNK_SIZE, 3)
}

fn handle_control_message(payload: &[u8], msg_type: u8) -> Option<usize> {
    match msg_type {
        0x01 if payload.len() >= 4 => {
            let new_size = ((payload[0] as usize) << 24)
                | ((payload[1] as usize) << 16)
                | ((payload[2] as usize) << 8)
                | payload[3] as usize;
            Some(new_size)
        }
        _ => None,
    }
}

async fn read_more(
    read_half: &mut tokio::net::tcp::OwnedReadHalf,
    buf: &mut BytesMut,
) -> Result<bool> {
    let mut read_buf = [0u8; 4096];
    let len = read_half.read(&mut read_buf).await?;
    if len == 0 {
        return Ok(false);
    }
    buf.extend_from_slice(&read_buf[..len]);
    Ok(true)
}

async fn wait_for_play_start(
    read_half: &mut tokio::net::tcp::OwnedReadHalf,
    buf: &mut BytesMut,
    chunk_states: &mut HashMap<u32, ChunkAssembler>,
    chunk_headers: &mut HashMap<u32, ChunkHeader>,
    chunk_size: &mut usize,
    manager: &StreamManager,
    local_stream_id: &str,
) -> Result<()> {
    for _ in 0..200 {
        let mut play_started = false;

        while let Some(msg) = chunk::parse_chunks(buf, chunk_states, chunk_headers, *chunk_size) {
            if let Some(new_size) = handle_control_message(&msg.payload, msg.msg_type) {
                *chunk_size = new_size;
            }
            if msg.msg_type == 0x09 {
                extract_and_store_sps_pps(&msg.payload, manager, local_stream_id);
            }
            if msg.msg_type == 0x14 || msg.msg_type == 0x11 {
                if let Ok((cmd, args)) = amf0::parse_command(&msg.payload) {
                    if cmd == "onStatus" {
                        if let Some(Amf0Value::Object(info)) = args.get(2) {
                            if let Some(Amf0Value::String(code)) = info.get("code") {
                                info!("[RTMP Puller] onStatus: {}", code);
                                if code == "NetStream.Play.Start" {
                                    play_started = true;
                                    break;
                                }
                                if code.contains("Failed") || code.contains("NotFound") {
                                    return Err(anyhow!("Play failed: {}", code));
                                }
                            }
                        }
                    } else if cmd == "_error" {
                        return Err(anyhow!("Play _error"));
                    }
                }
            }
        }

        if play_started {
            return Ok(());
        }

        if !read_more(read_half, buf).await? {
            return Err(anyhow!("Connection closed waiting for play start"));
        }
    }
    warn!("[RTMP Puller] No explicit Play.Start, continuing anyway");
    Ok(())
}

async fn wait_for_result(
    read_half: &mut tokio::net::tcp::OwnedReadHalf,
    buf: &mut BytesMut,
    chunk_states: &mut HashMap<u32, ChunkAssembler>,
    chunk_headers: &mut HashMap<u32, ChunkHeader>,
    chunk_size: &mut usize,
    tx_id: f64,
    phase: &str,
) -> Result<()> {
    for _ in 0..200 {
        let mut found = false;

        while let Some(msg) = chunk::parse_chunks(buf, chunk_states, chunk_headers, *chunk_size) {
            if let Some(new_size) = handle_control_message(&msg.payload, msg.msg_type) {
                *chunk_size = new_size;
            }
            if msg.msg_type == 0x14 || msg.msg_type == 0x11 {
                if let Ok((cmd, args)) = amf0::parse_command(&msg.payload) {
                    debug!("[RTMP Puller] recv cmd={} during {}", cmd, phase);
                    if cmd == "_result" {
                        if let Some(Amf0Value::Number(id)) = args.first() {
                            if (*id - tx_id).abs() < 0.001 {
                                info!("[RTMP Puller] {} _result OK", phase);
                                found = true;
                                break;
                            }
                        }
                    } else if cmd == "_error" {
                        return Err(anyhow!("{} failed: _error", phase));
                    } else if cmd == "onStatus" {
                        if let Some(Amf0Value::Object(info)) = args.get(2) {
                            if let Some(Amf0Value::String(code)) = info.get("code") {
                                if code.contains("Failed") || code.contains("NotFound") {
                                    return Err(anyhow!("{} onStatus: {}", phase, code));
                                }
                            }
                        }
                    }
                }
            }
        }

        if found {
            return Ok(());
        }

        if !read_more(read_half, buf).await? {
            return Err(anyhow!("Connection closed waiting for {} _result", phase));
        }
    }
    Err(anyhow!("Timeout waiting for {} _result", phase))
}

async fn wait_for_create_stream(
    read_half: &mut tokio::net::tcp::OwnedReadHalf,
    buf: &mut BytesMut,
    chunk_states: &mut HashMap<u32, ChunkAssembler>,
    chunk_headers: &mut HashMap<u32, ChunkHeader>,
    chunk_size: &mut usize,
    tx_id: f64,
) -> Result<f64> {
    for _ in 0..200 {
        let mut stream_id_result = None;

        while let Some(msg) = chunk::parse_chunks(buf, chunk_states, chunk_headers, *chunk_size) {
            if let Some(new_size) = handle_control_message(&msg.payload, msg.msg_type) {
                *chunk_size = new_size;
            }
            if msg.msg_type == 0x14 || msg.msg_type == 0x11 {
                if let Ok((cmd, args)) = amf0::parse_command(&msg.payload) {
                    if cmd == "_result" {
                        if let Some(Amf0Value::Number(id)) = args.first() {
                            if (*id - tx_id).abs() < 0.001 {
                                if let Some(Amf0Value::Number(stream_id)) = args.get(2) {
                                    stream_id_result = Some(*stream_id);
                                    break;
                                }
                            }
                        }
                    } else if cmd == "_error" {
                        return Err(anyhow!("createStream _error"));
                    }
                }
            }
        }

        if let Some(stream_id) = stream_id_result {
            info!("[RTMP Puller] createStream -> id={}", stream_id);
            return Ok(stream_id);
        }

        if !read_more(read_half, buf).await? {
            return Err(anyhow!("Connection closed waiting for createStream"));
        }
    }
    Err(anyhow!("Timeout waiting for createStream _result"))
}

fn extract_and_store_sps_pps(data: &[u8], manager: &StreamManager, stream_id: &str) {
    if data.len() < 2 || data[1] != 0x00 || data.len() <= 13 {
        return;
    }
    let config_data = &data[5..];
    if config_data.len() <= 7 {
        return;
    }
    let num_sps = (config_data[5] & 0x1F) as usize;
    let mut offset = 6;
    let mut sps_data = Vec::new();
    for _ in 0..num_sps {
        if offset + 2 > config_data.len() {
            break;
        }
        let sps_len = ((config_data[offset] as usize) << 8) | config_data[offset + 1] as usize;
        offset += 2;
        if offset + sps_len > config_data.len() {
            break;
        }
        sps_data = config_data[offset..offset + sps_len].to_vec();
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
            let pps_len = ((config_data[offset] as usize) << 8) | config_data[offset + 1] as usize;
            offset += 2;
            if offset + pps_len > config_data.len() {
                break;
            }
            pps_data = config_data[offset..offset + pps_len].to_vec();
            offset += pps_len;
        }
    }
    if !sps_data.is_empty() {
        info!(
            "[RTMP Puller] Stored SPS ({} bytes) PPS ({} bytes) for stream {}",
            sps_data.len(),
            pps_data.len(),
            stream_id
        );
        manager.set_stream_sps_pps(stream_id, sps_data, pps_data);
    }
}

fn rtmp_message_to_frame(msg: &chunk::RtmpMessage, stream_id: &str) -> Option<MediaFrame> {
    match msg.msg_type {
        0x09 => video_payload_to_frame(&msg.payload, stream_id, msg.timestamp),
        0x08 => audio_payload_to_frame(&msg.payload, stream_id, msg.timestamp),
        _ => None,
    }
}

fn video_payload_to_frame(data: &[u8], stream_id: &str, timestamp: u32) -> Option<MediaFrame> {
    if data.len() < 2 {
        return None;
    }
    let avc_packet_type = data[1];
    if avc_packet_type != 0x01 {
        return None;
    }

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

    Some(
        MediaFrame::new(
            stream_id.to_string(),
            0,
            timestamp as u64,
            Bytes::from(annex_b),
            is_keyframe,
            CodecType::H264,
        )
        .with_clock_rate(MILLISECOND_CLOCK_RATE),
    )
}

fn audio_payload_to_frame(data: &[u8], stream_id: &str, timestamp: u32) -> Option<MediaFrame> {
    if data.is_empty() {
        return None;
    }
    if data.len() > 1 && data[1] == 0x00 {
        return None; // sequence header
    }

    let audio_data = if data.len() > 1 && data[0] == 0xAF && data[1] == 0x01 {
        &data[2..]
    } else if data.len() > 1 {
        &data[1..]
    } else {
        data
    };

    if audio_data.is_empty() {
        return None;
    }

    Some(
        MediaFrame::new(
            stream_id.to_string(),
            1,
            timestamp as u64,
            Bytes::copy_from_slice(audio_data),
            false,
            CodecType::AAC,
        )
        .with_clock_rate(MILLISECOND_CLOCK_RATE),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rtmp_url() {
        let u = parse_rtmp_url("rtmp://127.0.0.1:1935/live/stream1").unwrap();
        assert_eq!(u.host, "127.0.0.1");
        assert_eq!(u.port, 1935);
        assert_eq!(u.app, "live");
        assert_eq!(u.stream_name, "stream1");
        assert_eq!(u.tc_url, "rtmp://127.0.0.1:1935/live");
    }
}
