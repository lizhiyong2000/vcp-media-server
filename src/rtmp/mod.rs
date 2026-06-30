pub mod amf0;
pub mod chunk;
pub mod session;
pub mod puller;

pub use puller::RtmpPuller;

use anyhow::Result;
use bytes::{Bytes, BytesMut, Buf};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn, error, debug};

use crate::core::{StreamManager, CodecType, MediaFrame, StreamSourceMode, StreamProtocol, StreamStatus, drain_broadcast_lag, recv_flv_batch, default_live_tracks};
use session::{RtmpSession, SessionState};
use chunk::RtmpMessage;

/// 将 RTMP 消息类型 ID 转为可读名称
fn msg_type_name(t: u8) -> &'static str {
    match t {
        0x01 => "SetChunkSize",
        0x02 => "Abort",
        0x03 => "Acknowledgement",
        0x04 => "UserControl",
        0x05 => "WindowAckSize",
        0x06 => "SetPeerBandwidth",
        0x08 => "Audio",
        0x09 => "Video",
        0x0F => "DataAMF3",
        0x10 => "SharedObjAMF3",
        0x11 => "CommandAMF3",
        0x12 => "DataAMF0",
        0x13 => "SharedObjAMF0",
        0x14 => "CommandAMF0",
        0x16 => "Aggregate",
        _ => "Unknown",
    }
}

/// 将视频帧类型转为可读名称
fn video_frame_type(b: u8) -> &'static str {
    match b & 0xF0 {
        0x10 => "KeyFrame",
        0x20 => "InterFrame",
        0x30 => "Disposable",
        0x40 => "Generated",
        0x50 => "Command",
        _ => "Unknown",
    }
}

/// 将视频编解码器转为可读名称
fn video_codec_name(b: u8) -> &'static str {
    match b & 0x0F {
        0x07 => "H264",
        0x0C => "H265",
        _ => "Unknown",
    }
}

/// 将 AMF0 值格式化为可读字符串
fn fmt_amf0(val: &amf0::Amf0Value) -> String {
    match val {
        amf0::Amf0Value::Number(n) => format!("{}", n),
        amf0::Amf0Value::String(s) => format!("\"{}\"", s),
        amf0::Amf0Value::Boolean(b) => format!("{}", b),
        amf0::Amf0Value::Null => "null".to_string(),
        amf0::Amf0Value::Undefined => "undefined".to_string(),
        amf0::Amf0Value::Object(map) | amf0::Amf0Value::EcmaArray(map) => {
            let pairs: Vec<String> = map.iter()
                .map(|(k, v)| format!("{}={}", k, fmt_amf0(v)))
                .collect();
            format!("{{{}}}", pairs.join(", "))
        }
    }
}

struct RtmpConnection {
    stream_manager: Arc<StreamManager>,
    hls_server: Option<Arc<crate::hls::HlsServer>>,
    session: RtmpSession,
    writer: Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    chunk_size: usize,
    chunk_states: HashMap<u32, chunk::ChunkAssembler>,
    chunk_message_headers: HashMap<u32, chunk::ChunkHeader>,
    frames_received: usize,
    is_publishing: bool,
    stream_id: String,
    play_abort: Option<tokio::task::AbortHandle>,
}

impl RtmpConnection {
    fn new(
        stream_manager: Arc<StreamManager>,
        hls_server: Option<Arc<crate::hls::HlsServer>>,
        writer: Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
        peer_addr: &str,
    ) -> Self {
        Self {
            stream_manager,
            hls_server,
            session: RtmpSession::new(peer_addr),
            writer,
            chunk_size: 4096,
            chunk_states: HashMap::new(),
            chunk_message_headers: HashMap::new(),
            frames_received: 0,
            is_publishing: false,
            stream_id: String::new(),
            play_abort: None,
        }
    }
}

pub struct RtmpServer {
    stream_manager: Arc<StreamManager>,
    port: u16,
    hls_server: Option<Arc<crate::hls::HlsServer>>,
}

impl RtmpServer {
    pub fn new(
        stream_manager: Arc<StreamManager>,
        port: u16,
        hls_server: Option<Arc<crate::hls::HlsServer>>,
    ) -> Self {
        Self { stream_manager, port, hls_server }
    }

    pub async fn start(&self) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.port);
        info!("[RTMP] Initializing RTMP server on {}", addr);
        info!("[RTMP] Binding to {}:{}", addr.split(':').next().unwrap_or("0.0.0.0"), self.port);

        let listener = TcpListener::bind(&addr).await?;
        info!("[RTMP] Successfully bound to address {}", addr);
        info!("[RTMP] RTMP server is ready and listening");
        info!("[RTMP] Accepting connections on rtmp://{}:{}/<stream_name>", addr.split(':').next().unwrap_or("0.0.0.0"), self.port);

        loop {
            match listener.accept().await {
                Ok((socket, peer_addr)) => {
                    info!("[RTMP] =============================");
                    info!("[RTMP] New connection from {}", peer_addr);
                    info!("[RTMP] Connection ID: {:?}", socket.peer_addr());
                    let manager = self.stream_manager.clone();
                    let hls = self.hls_server.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_connection(socket, manager, hls, peer_addr).await {
                            error!("[RTMP] Connection error from {}: {}", peer_addr, e);
                        }
                    });
                }
                Err(e) => {
                    error!("[RTMP] Failed to accept connection: {}", e);
                    warn!("[RTMP] Waiting for new connections...");
                }
            }
        }
    }

    async fn handle_connection(
        socket: TcpStream,
        manager: Arc<StreamManager>,
        hls_server: Option<Arc<crate::hls::HlsServer>>,
        peer_addr: std::net::SocketAddr,
    ) -> Result<()> {
        let (mut reader, writer) = socket.into_split();
        let writer = Arc::new(Mutex::new(writer));
        let mut buf = BytesMut::with_capacity(8192);
        let mut conn = RtmpConnection::new(manager.clone(), hls_server, writer.clone(), &peer_addr.to_string());
        let mut chunk_size: usize = 128;
        let mut bytes_received = 0;
        let mut handshake_done = false;
        let mut handshake_first_call = true;

        info!("[RTMP] [{}] Starting connection handling", peer_addr);

        loop {
            let mut read_buf = [0u8; 8192];
            let len = reader.read(&mut read_buf).await?;
            
            if len == 0 {
                info!("[RTMP] [{}] Connection closed, bytes={}, frames={}", peer_addr, bytes_received, conn.frames_received);
                if let Some(handle) = conn.play_abort.take() {
                    handle.abort();
                }
                break;
            }

            bytes_received += len;
            buf.extend_from_slice(&read_buf[..len]);
            debug!("[RTMP] [{}] Read {} bytes, buf_len={}, chunk_size={}", peer_addr, len, buf.len(), chunk_size);

            if !handshake_done {
                let mut writer_guard = writer.lock().await;
                if Self::handle_handshake(&mut buf, &mut writer_guard, peer_addr, handshake_first_call).await? {
                    handshake_done = true;
                    conn.session.state = SessionState::Connected;
                } else {
                    handshake_first_call = false;
                    continue;
                }
            }

            // Parse chunks using the chunk module
            let initial_buf_len = buf.len();
            let mut messages_parsed = 0;
            let mut pending_chunk_size = None;
            while let Some(msg) = chunk::parse_chunks(&mut buf, &mut conn.chunk_states, &mut conn.chunk_message_headers, chunk_size) {
                messages_parsed += 1;
                debug!("[RTMP] [{}] <<< RECV [{}] 0x{:02x} ts={} {}bytes",
                    peer_addr, msg_type_name(msg.msg_type), msg.msg_type,
                    msg.timestamp, msg.payload.len());
                if let Some(new_size) = Self::handle_rtmp_message(&mut conn, msg, peer_addr).await? {
                    // Defer chunk_size update until all messages in this batch are parsed
                    // because remaining data was encoded with the old chunk_size
                    pending_chunk_size = Some(new_size);
                }
            }
            // Apply deferred chunk_size update
            if let Some(new_size) = pending_chunk_size {
                chunk_size = new_size;
                info!("[RTMP] [{}] chunk_size updated to {} (after batch)", peer_addr, chunk_size);
            }
            // Always log buffer state after parsing
            if buf.len() > 0 {
                let preview_len = std::cmp::min(64, buf.len());
                let preview: Vec<String> = buf[..preview_len].iter().map(|b| format!("{:02x}", *b)).collect();
                debug!("[RTMP] [{}] After parse: buf={} bytes, messages={}, chunk_size={}, preview: {}",
                    peer_addr, buf.len(), messages_parsed, chunk_size, preview.join(" "));
            } else {
                debug!("[RTMP] [{}] After parse: buf empty, messages={}", peer_addr, messages_parsed);
            }
        }

        Ok(())
    }

    async fn handle_handshake(buf: &mut BytesMut, writer: &mut tokio::net::tcp::OwnedWriteHalf, peer_addr: std::net::SocketAddr, first_call: bool) -> Result<bool> {
        if buf.len() < 1537 {
            return Ok(false);
        }

        if first_call && buf[0] != 0x03 {
            warn!("[RTMP] [{}] Invalid RTMP version: 0x{:02X}", peer_addr, buf[0]);
            buf.clear();
            return Ok(false);
        }

        if first_call {
            info!("[RTMP] [{}] Received RTMP version 0x{:02X}, starting handshake", peer_addr, buf[0]);

            let mut response = vec![0x03];
            response.extend_from_slice(&[0u8; 1536]);
            response.extend_from_slice(&buf[1..1537]);

            writer.write_all(&response).await?;
            info!("[RTMP] [{}] Sent handshake response ({} bytes)", peer_addr, response.len());

            buf.advance(1537);
        }

        if buf.len() >= 1536 {
            buf.advance(1536);
            info!("[RTMP] [{}] Handshake completed", peer_addr);
            return Ok(true);
        }

        Ok(false)
    }

    async fn handle_rtmp_message(
        conn: &mut RtmpConnection,
        msg: RtmpMessage,
        peer_addr: std::net::SocketAddr,
    ) -> Result<Option<usize>> {
        let mut new_chunk_size = None;
        match msg.msg_type {
            // AMF0 Command
            0x14 | 0x12 => {
                if let Ok((command, args)) = amf0::parse_command(&msg.payload) {
                    let args_str: Vec<String> = args.iter().map(|a| fmt_amf0(a)).collect();
                    info!("[RTMP] [{}] <<< CMD  {}({})",
                        peer_addr, command, args_str.join(", "));
                    Self::handle_amf0_command(conn, &command, &args, peer_addr).await?;
                } else {
                    error!("[RTMP] [{}] <<< CMD  parse FAILED ({}bytes)", peer_addr, msg.payload.len());
                }
            }
            // Video data
            0x09 => {
                let data = &msg.payload;
                if data.is_empty() { return Ok(None); }
                let frame_type_str = video_frame_type(data[0]);
                let codec_str = video_codec_name(data[0]);
                let avc_packet_type = if data.len() > 1 { data[1] } else { 0xFF };

                match avc_packet_type {
                    0x00 => {
                        // AVC sequence header (SPS/PPS)
                        info!("[RTMP] [{}] <<< VIDEO AVC SequenceHeader ({}+{})", peer_addr, frame_type_str, codec_str);
                        if data.len() > 13 {
                            let config_data = &data[5..]; // skip frame_type + avc_type + composition_time
                            if config_data.len() > 7 {
                                let num_sps = (config_data[5] & 0x1F) as usize;
                                let mut offset = 6;
                                let mut sps_data = Vec::new();
                                for _ in 0..num_sps {
                                    if offset + 2 > config_data.len() { break; }
                                    let sps_len = ((config_data[offset] as usize) << 8) | config_data[offset + 1] as usize;
                                    offset += 2;
                                    if offset + sps_len > config_data.len() { break; }
                                    sps_data = config_data[offset..offset + sps_len].to_vec();
                                    offset += sps_len;
                                }
                                let mut pps_data = Vec::new();
                                if offset < config_data.len() {
                                    let num_pps = config_data[offset] & 0x1F;
                                    offset += 1;
                                    for _ in 0..num_pps as usize {
                                        if offset + 2 > config_data.len() { break; }
                                        let pps_len = ((config_data[offset] as usize) << 8) | config_data[offset + 1] as usize;
                                        offset += 2;
                                        if offset + pps_len > config_data.len() { break; }
                                        pps_data = config_data[offset..offset + pps_len].to_vec();
                                        offset += pps_len;
                                    }
                                }
                                if !sps_data.is_empty() {
                                    conn.stream_manager.set_stream_sps_pps(&conn.stream_id, sps_data, pps_data);
                                }
                            }
                        }
                    }
                    0x01 => {
                        // Video NALU (AVC)
                        conn.frames_received += 1;
                        let is_keyframe = (data[0] & 0xF0) == 0x10;
                        let avcc_payload = &data[5..];

                        if conn.frames_received <= 3 || is_keyframe && conn.frames_received % 30 == 0 {
                            debug!("[RTMP] [{}] <<< VIDEO {} {} ts={} size={} frame#{}",
                                peer_addr, frame_type_str, codec_str,
                                msg.timestamp, avcc_payload.len(), conn.frames_received);
                        }
                        if conn.is_publishing {
                            // Convert AVCC format to Annex B (add start codes)
                            let mut annex_b = Vec::with_capacity(avcc_payload.len());
                            let mut offset = 0;
                            while offset + 4 <= avcc_payload.len() {
                                let nalu_len = ((avcc_payload[offset] as usize) << 24)
                                    | ((avcc_payload[offset + 1] as usize) << 16)
                                    | ((avcc_payload[offset + 2] as usize) << 8)
                                    | (avcc_payload[offset + 3] as usize);
                                offset += 4;
                                if offset + nalu_len > avcc_payload.len() { break; }
                                annex_b.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                                annex_b.extend_from_slice(&avcc_payload[offset..offset + nalu_len]);
                                offset += nalu_len;
                            }

                            if !annex_b.is_empty() {
                                let frame = MediaFrame::new(
                                    conn.stream_id.clone(), 0, msg.timestamp as u64,
                                    Bytes::from(annex_b), is_keyframe, CodecType::H264,
                                );
                                conn.stream_manager.publish_frame(frame);
                            }
                        }
                    }
                    _ => {
                        debug!("[RTMP] [{}] <<< VIDEO unknown avc_packet_type=0x{:02X} ({}bytes)",
                            peer_addr, avc_packet_type, data.len());
                    }
                }
            }
            // Audio data
            0x08 => {
                let data = &msg.payload;
                if data.is_empty() { return Ok(None); }
                let sound_format = (data[0] >> 4) & 0x0F;
                let sound_rate = (data[0] >> 2) & 0x03;
                let sound_size = (data[0] >> 1) & 0x01;
                let sound_type = data[0] & 0x01;
                let is_header = data.len() > 1 && data[1] == 0x00;
                if is_header {
                    info!("[RTMP] [{}] <<< AUDIO SequenceHeader format={} rate={}Hz bits={} ch={}",
                        peer_addr,
                        match sound_format { 0x0A => "AAC", 0x02 => "MP3", _ => "Other" },
                        match sound_rate { 0 => "5.5k", 1 => "11k", 2 => "22k", 3 => "44k", _ => "?" },
                        if sound_size == 1 { 16 } else { 8 },
                        if sound_type == 1 { "stereo" } else { "mono" });
                } else {
                    debug!("[RTMP] [{}] <<< AUDIO ts={} size={} frame#{}",
                        peer_addr, msg.timestamp, data.len(), conn.frames_received);
                }
                conn.frames_received += 1;
                if conn.is_publishing && !is_header {
                    // Skip audio tag header; do not mux AAC sequence header as media
                    let audio_data = if data.len() > 1 && data[0] == 0xAF && data[1] == 0x01 {
                        &data[2..]
                    } else if data.len() > 1 {
                        &data[1..]
                    } else {
                        data
                    };
                    let frame = MediaFrame::new(
                        conn.stream_id.clone(), 1, msg.timestamp as u64,
                        Bytes::copy_from_slice(audio_data), false, CodecType::AAC,
                    );
                    conn.stream_manager.publish_frame(frame);
                }
            }
            // Protocol control: Set Chunk Size
            0x01 => {
                if msg.payload.len() >= 4 {
                    let new_size = ((msg.payload[0] as usize) << 24)
                        | ((msg.payload[1] as usize) << 16)
                        | ((msg.payload[2] as usize) << 8)
                        | msg.payload[3] as usize;
                    conn.chunk_size = new_size;
                    new_chunk_size = Some(new_size);
                    info!("[RTMP] [{}] <<< SetChunkSize({})", peer_addr, new_size);
                }
            }
            // Window Ack Size
            0x05 => {
                if msg.payload.len() >= 4 {
                    let ack_size = ((msg.payload[0] as u32) << 24)
                        | ((msg.payload[1] as u32) << 16)
                        | ((msg.payload[2] as u32) << 8)
                        | msg.payload[3] as u32;
                    info!("[RTMP] [{}] <<< WindowAckSize({})", peer_addr, ack_size);
                }
            }
            // Set Peer Bandwidth
            0x06 => {
                if msg.payload.len() >= 5 {
                    let bw = ((msg.payload[0] as u32) << 24)
                        | ((msg.payload[1] as u32) << 16)
                        | ((msg.payload[2] as u32) << 8)
                        | msg.payload[3] as u32;
                    let limit = match msg.payload[4] { 0 => "Hard", 1 => "Soft", 2 => "Dynamic", _ => "?" };
                    info!("[RTMP] [{}] <<< SetPeerBandwidth({}, {})", peer_addr, bw, limit);
                }
            }
            _ => {
                info!("[RTMP] [{}] <<< Unknown msg type={} (0x{:02x}) {}bytes",
                    peer_addr, msg_type_name(msg.msg_type), msg.msg_type, msg.payload.len());
            }
        }
        Ok(new_chunk_size)
    }

    async fn handle_amf0_command(
        conn: &mut RtmpConnection,
        command: &str,
        args: &[amf0::Amf0Value],
        peer_addr: std::net::SocketAddr,
    ) -> Result<()> {
        match command {
            "connect" => {
                info!("[RTMP] [{}] --- handling connect", peer_addr);
                 conn.stream_id = conn.session.handle_connect(args);

                // Send Window Ack Size
                let ack = chunk::encode_window_ack_size(2500000);
                let mut w = conn.writer.lock().await;
                w.write_all(&ack).await?;

                // Send Set Peer Bandwidth
                let bw = chunk::encode_set_peer_bandwidth(2500000, 2);
                w.write_all(&bw).await?;

                // Send Set Chunk Size - tells client server will use new size
                let cs = chunk::encode_set_chunk_size(conn.session.chunk_size as u32);
                w.write_all(&cs).await?;

                // Send _result using new chunk_size (SetChunkSize takes effect immediately for sender)
                let result = session::build_result_response(conn.session.transaction_id, "Connect");
                let response = chunk::encode_message(0x14, 0, 0, &result, conn.session.chunk_size, 3);
                w.write_all(&response).await?;
                w.flush().await?;
                info!("[RTMP] [{}] >>> SENT _result(connect) + WindowAck + PeerBW + SetChunkSize({})",
                    peer_addr, conn.session.chunk_size);
            }
            "releaseStream" | "FCPublish" | "FCUnpublish" => {
                debug!("[RTMP] [{}] --- {} (ack)", peer_addr, command);
            }
            "createStream" => {
                let stream_id = conn.session.handle_create_stream(args);
                info!("[RTMP] [{}] --- createStream -> stream_id={}", peer_addr, stream_id);

                let mut result_values = vec![
                    amf0::Amf0Value::String("_result".to_string()),
                    amf0::Amf0Value::Number(conn.session.transaction_id),
                    amf0::Amf0Value::Null,
                    amf0::Amf0Value::Number(stream_id as f64),
                ];
                let result = amf0::encode(&result_values);
                let response = chunk::encode_message(0x14, 0, 0, &result, conn.session.chunk_size, 3);
                let mut w = conn.writer.lock().await;
                w.write_all(&response).await?;
                info!("[RTMP] [{}] >>> SENT _result(createStream) stream_id={}", peer_addr, stream_id);
            }
            "publish" => {
                let stream_name = conn.session.handle_publish(args);
                conn.is_publishing = true;
                conn.stream_id = stream_name.clone();

                if conn.stream_manager.get_stream(&stream_name).is_none() {
                    conn.stream_manager.create_stream(&stream_name, StreamSourceMode::Push, StreamProtocol::RTMP, None);
                    conn.stream_manager.set_stream_tracks(&stream_name, default_live_tracks());
                    let _ = conn.stream_manager.set_unpublished(&stream_name);
                    info!("[RTMP] [{}] Created new stream: '{}'", peer_addr, stream_name);

                } else if conn.stream_manager.get_stream(&stream_name).map(|s| s.tracks.is_empty()).unwrap_or(false) {
                    conn.stream_manager.set_stream_tracks(&stream_name, default_live_tracks());
                }

                // // ========== 新增：先发送 Stream Begin ==========
                // let mut stream_begin_data = Vec::new();
                // stream_begin_data.extend_from_slice(&0u16.to_be_bytes()); // Event Type = 0 (Stream Begin)
                // stream_begin_data.extend_from_slice(&(stream_id as u32).to_be_bytes()); // Stream ID
                //
                // let stream_begin_msg = chunk::encode_message(
                //     4,        // message type = 4 (User Control Message)
                //     0,        // stream id = 0
                //     0,        // timestamp
                //     &stream_begin_data,
                //     conn.session.chunk_size,
                //     2,        // chunk stream id = 2 (协议规定控制消息用)
                // );
                // {
                //     let mut w = conn.writer.lock().await;
                //     w.write_all(&stream_begin_msg).await?;
                // }
                // =============================================

                let status = session::build_on_status("NetStream.Publish.Start", "Publishing started.", "status");
                let response = chunk::encode_message(0x14, 0, conn.session.server_stream_id, &status, conn.session.chunk_size, 3);
                let mut w = conn.writer.lock().await;
                w.write_all(&response).await?;
                info!("[RTMP] [{}] >>> SENT onStatus(NetStream.Publish.Start) stream='{}'", peer_addr, stream_name);

                // conn.stream_manager.add_stream(&stream_name);
                conn.stream_manager.set_stream_broadcast(&stream_name);
                _= conn.stream_manager.set_publishing(&stream_name);

                if let Some(hls) = conn.hls_server.clone() {
                    let name = stream_name.clone();
                    tokio::spawn(async move {
                        let _ = hls.restart_stream(&name).await;
                    });
                }
            }
            "play" => {
                let play_stream_id = conn.session.handle_play(args);
                conn.stream_id = play_stream_id.clone();
                info!("[RTMP] [{}] --- play stream='{}'", peer_addr, play_stream_id);

                if conn.stream_manager.get_stream(&play_stream_id).is_none() {
                    warn!("[RTMP] [{}] Stream '{}' does not exist", peer_addr, play_stream_id);
                    let status = session::build_on_status("NetStream.Play.StreamNotFound", "Stream not found", "error");
                    let response = chunk::encode_message(0x14, 0, conn.session.server_stream_id, &status, conn.session.chunk_size, 3);
                    let mut w = conn.writer.lock().await;
                    w.write_all(&response).await?;

                    info!("[RTMP] [{}] >>> SENT onStatus('NetStream.Play.StreamNotFound') stream='{}'", peer_addr, play_stream_id);
                    return Ok(());
                }





                // Send AVC sequence header if available
                if let Some(stream) = conn.stream_manager.get_stream(&play_stream_id) {

                    if stream.status != StreamStatus::Publishing {
                        warn!("[RTMP] [{}] Stream '{}' not publishing", peer_addr, play_stream_id);
                        let status = session::build_on_status("NetStream.Play.StreamNotFound", "Stream not found" , "error");
                        let response = chunk::encode_message(0x14, 0, conn.session.server_stream_id, &status, conn.session.chunk_size, 3);
                        let mut w = conn.writer.lock().await;
                        w.write_all(&response).await?;

                        info!("[RTMP] [{}] >>> SENT onStatus('NetStream.Play.StreamNotFound') stream='{}'", peer_addr, play_stream_id);
                        return Ok(());
                    }


                    // Start forwarding frames
                    let writer_clone = conn.writer.clone();
                    let stream_id_clone = play_stream_id.clone();
                    let chunk_size_val = conn.session.chunk_size;
                    let server_stream_id = conn.session.server_stream_id;
                    if let Some(handle) = conn.play_abort.take() {
                        handle.abort();
                    }
                    if let Some(mut rx) = conn.stream_manager.subscribe(&play_stream_id) {
                        info!("[RTMP] [{}] Subscribed to stream '{}'", peer_addr, play_stream_id);

                        let dropped = drain_broadcast_lag(&mut rx);
                        if dropped > 0 {
                            info!(
                                "[RTMP] [{}] Flushed {} stale frames before live edge",
                                peer_addr, dropped
                            );
                        }

                        // Send Stream Begin
                        let mut w = conn.writer.lock().await;

                        // Send onStatus(NetStream.Play.Start)
                        let status = session::build_on_status("NetStream.Play.Start", "Play started.", "status");
                        let response = chunk::encode_message(0x14, 0, conn.session.server_stream_id, &status, conn.session.chunk_size, chunk::CSID_COMMAND);
                        w.write_all(&response).await?;

                        if let (Some(ref sps), Some(ref pps)) = (&stream.sps, &stream.pps) {
                            let avc_header = session::build_avc_sequence_header(sps, pps);
                            let header_msg = chunk::encode_message(
                                0x09, 0, conn.session.server_stream_id, &avc_header,
                                conn.session.chunk_size, chunk::CSID_VIDEO,
                            );
                            w.write_all(&header_msg).await?;
                            info!("[RTMP] [{}] Sent AVC sequence header", peer_addr);
                        }
                        // Send AAC sequence header
                        let aac_header = session::build_aac_sequence_header();
                        let aac_msg = chunk::encode_message(
                            0x08, 0, conn.session.server_stream_id, &aac_header,
                            conn.session.chunk_size, chunk::CSID_AUDIO,
                        );
                        w.write_all(&aac_msg).await?;

                        info!("[RTMP] [{}] >>> SENT play response (onStatus+AVCHeader+AACHeader)", peer_addr);
                        drop(w);

                        let peer_log = peer_addr.to_string();
                        let manager_clone = conn.stream_manager.clone();
                        let handle = tokio::spawn(async move {
                            let mut pending = crate::rtsp::play_egress::prime_rtsp_play_rx(
                                &mut rx,
                                &manager_clone,
                                &stream_id_clone,
                            )
                            .await;
                            let mut video_streaming = false;
                            let mut frames_sent = 0u64;
                            let mut clock = session::RtmpPlayClock::default();
                            loop {
                                let frames = if let Some(frame) = pending.take() {
                                    vec![frame]
                                } else {
                                    match recv_flv_batch(&mut rx).await {
                                        Ok(frames) => frames,
                                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                            info!(
                                                "[RTMP] [{}] Play lagged {} frames — jump to live edge",
                                                peer_log, n
                                            );
                                            drain_broadcast_lag(&mut rx);
                                            continue;
                                        }
                                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                    }
                                };
                                for frame in frames {
                                    if frame.codec == CodecType::Opus || frame.codec == CodecType::G711 {
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
                                    if frames_sent == 0 {
                                        info!(
                                            "[RTMP] >>> SEND first frame: codec={:?} keyframe={} raw_ts={} size={}",
                                            frame.codec, frame.is_keyframe, frame.timestamp, frame.data.len()
                                        );
                                    }
                                    let data = match frame.codec {
                                        CodecType::H264 | CodecType::H265 => {
                                            session::frame_to_rtmp_video(&frame)
                                        }
                                        CodecType::AAC => session::frame_to_rtmp_audio(&frame),
                                        _ => continue,
                                    };
                                    if data.is_empty() {
                                        continue;
                                    }
                                    let (msg_type, csid) = match frame.codec {
                                        CodecType::H264 | CodecType::H265 => (0x09u8, chunk::CSID_VIDEO),
                                        _ => (0x08u8, chunk::CSID_AUDIO),
                                    };
                                    let rtmp_ts = clock.map(&frame);
                                    let rtmp_msg = chunk::encode_message(
                                        msg_type,
                                        rtmp_ts,
                                        server_stream_id,
                                        &data,
                                        chunk_size_val,
                                        csid,
                                    );
                                    let mut guard = writer_clone.lock().await;
                                    if guard.write_all(&rtmp_msg).await.is_err() {
                                        info!("[RTMP] [{}] Play client disconnected", peer_log);
                                        return;
                                    }
                                    frames_sent += 1;
                                    if frames_sent % 100 == 0 {
                                        info!(
                                            "[RTMP] >>> SEND {} frames to player (codec={:?} rtmp_ts={})",
                                            frames_sent, frame.codec, rtmp_ts
                                        );
                                    }
                                }
                            }
                        });
                        conn.play_abort = Some(handle.abort_handle());
                    }else{
                        warn!("[RTMP] [{}] Subscribe to stream '{}' error", peer_addr, play_stream_id);
                        let status = session::build_on_status("NetStream.Play.StreamNotFound", "Stream not found", "error");
                        let response = chunk::encode_message(0x14, 0, conn.session.server_stream_id, &status, conn.session.chunk_size, 3);
                        let mut w = conn.writer.lock().await;
                        w.write_all(&response).await?;
                        info!("[RTMP] [{}] >>> SENT onStatus('NetStream.Play.StreamNotFound') stream='{}'", peer_addr, play_stream_id);
                        return Ok(());
                    }

                }


            }
            "deleteStream" => {
                info!("[RTMP] [{}] --- deleteStream", peer_addr);
                conn.is_publishing = false;
                conn.session.state = SessionState::Ready;
            }
            "closeStream" => {
                info!("[RTMP] [{}] --- closeStream", peer_addr);
                conn.is_publishing = false;
                conn.session.state = SessionState::Closing;
            }
            _ => {
                info!("[RTMP] [{}] --- unknown AMF0 command: '{}'", peer_addr, command);
            }
        }
        Ok(())
    }

    // Legacy methods kept for backward compatibility
    #[allow(dead_code)]
    fn parse_amf0_command(payload: &[u8], peer_addr: std::net::SocketAddr) -> Option<(String, HashMap<String, String>)> {
        if payload.is_empty() {
            return None;
        }

        let mut offset = 0;
        let mut args: HashMap<String, String> = HashMap::new();

        if offset + 1 >= payload.len() {
            return None;
        }

        let command = match payload[offset] {
            0x02 => {
                offset += 1;
                if offset + 2 > payload.len() {
                    return None;
                }
                let str_len = ((payload[offset] as usize) << 8) | payload[offset + 1] as usize;
                offset += 2;
                if offset + str_len > payload.len() {
                    return None;
                }
                let cmd = String::from_utf8_lossy(&payload[offset..offset + str_len]).to_string();
                offset += str_len;
                cmd
            }
            _ => return None,
        };

        info!("[RTMP] [{}] AMF0 command: '{}'", peer_addr, command);

        while offset < payload.len() {
            if offset + 1 > payload.len() {
                break;
            }

            let marker = payload[offset];
            
            match marker {
                0x02 => {
                    offset += 1;
                    if offset + 2 > payload.len() {
                        break;
                    }
                    let str_len = ((payload[offset] as usize) << 8) | payload[offset + 1] as usize;
                    offset += 2;
                    if offset + str_len > payload.len() {
                        break;
                    }
                    let key = String::from_utf8_lossy(&payload[offset..offset + str_len]).to_string();
                    offset += str_len;

                    if offset + 1 > payload.len() {
                        break;
                    }

                    let value = match payload[offset] {
                        0x02 => {
                            offset += 1;
                            if offset + 2 > payload.len() {
                                break;
                            }
                            let v_len = ((payload[offset] as usize) << 8) | payload[offset + 1] as usize;
                            offset += 2;
                            if offset + v_len > payload.len() {
                                break;
                            }
                            let val = String::from_utf8_lossy(&payload[offset..offset + v_len]).to_string();
                            offset += v_len;
                            val
                        }
                        0x00 => {
                            offset += 1;
                            "null".to_string()
                        }
                        0x01 => {
                            offset += 1;
                            if offset < payload.len() {
                                offset += 1;
                                payload[offset - 1].to_string()
                            } else {
                                break;
                            }
                        }
                        0x04 => {
                            offset += 1;
                            if offset + 4 > payload.len() {
                                break;
                            }
                            offset += 4;
                            let mut obj_args = HashMap::new();
                            while offset + 1 < payload.len() && payload[offset] != 0x09 {
                                if payload[offset] == 0x02 {
                                    offset += 1;
                                    if offset + 2 > payload.len() {
                                        break;
                                    }
                                    let k_len = ((payload[offset] as usize) << 8) | payload[offset + 1] as usize;
                                    offset += 2;
                                    if offset + k_len > payload.len() {
                                        break;
                                    }
                                    let k = String::from_utf8_lossy(&payload[offset..offset + k_len]).to_string();
                                    offset += k_len;

                                    if offset + 1 > payload.len() {
                                        break;
                                    }

                                    let v = match payload[offset] {
                                        0x02 => {
                                            offset += 1;
                                            if offset + 2 > payload.len() {
                                                break;
                                            }
                                            let v_len = ((payload[offset] as usize) << 8) | payload[offset + 1] as usize;
                                            offset += 2;
                                            if offset + v_len > payload.len() {
                                                break;
                                            }
                                            let val = String::from_utf8_lossy(&payload[offset..offset + v_len]).to_string();
                                            offset += v_len;
                                            val
                                        }
                                        _ => {
                                            offset += 1;
                                            "unknown".to_string()
                                        }
                                    };
                                    obj_args.insert(k, v);
                                } else {
                                    break;
                                }
                            }
                            offset += 1;
                            args.extend(obj_args);
                            continue;
                        }
                        _ => {
                            offset += 1;
                            "unknown".to_string()
                        }
                    };

                    args.insert(key, value);
                }
                0x00 => {
                    offset = offset.saturating_add(1);
                    break;
                }
                0x09 => {
                    offset = offset.saturating_add(1);
                    break;
                }
                _ => {
                    offset = offset.saturating_add(1);
                }
            }
        }

        info!("[RTMP] [{}] AMF0 args: {:?}", peer_addr, args);
        Some((command, args))
    }

    // Legacy response builders (now using session module)
    #[allow(dead_code)]
    fn make_connect_response() -> Vec<u8> {
        session::build_result_response(1.0, "Connect")
    }

    #[allow(dead_code)]
    fn make_create_stream_response() -> Vec<u8> {
        let values = vec![
            amf0::Amf0Value::String("_result".to_string()),
            amf0::Amf0Value::Number(2.0),
            amf0::Amf0Value::Null,
            amf0::Amf0Value::Number(1.0),
        ];
        amf0::encode(&values)
    }

    #[allow(dead_code)]
    fn make_publish_response() -> Vec<u8> {
        session::build_on_status("NetStream.Publish.Start", "Publishing started.", "status")
    }

    #[allow(dead_code)]
    fn make_play_response() -> Vec<u8> {
        session::build_on_status("NetStream.Play.Start", "Playback started.", "status")
    }

    #[allow(dead_code)]
    fn frame_to_rtmp_data(frame: &MediaFrame) -> Vec<u8> {
        match frame.codec {
            CodecType::H264 | CodecType::H265 => session::frame_to_rtmp_video(frame),
            CodecType::AAC | CodecType::Opus | CodecType::G711 => session::frame_to_rtmp_audio(frame),
            _ => Vec::new(),
        }
    }
}
