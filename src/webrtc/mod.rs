use anyhow::Result;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn, error};

use crate::core::{StreamManager, MediaFrame};

pub struct WebrtcServer {
    stream_manager: Arc<StreamManager>,
    port: u16,
}

impl WebrtcServer {
    pub fn new(stream_manager: Arc<StreamManager>, port: u16) -> Self {
        Self { stream_manager, port }
    }

    pub async fn start(&self) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.port);
        info!("[WebRTC] Initializing WebRTC signaling server on {}", addr);
        info!("[WebRTC] Binding to {}:{}", addr.split(':').next().unwrap_or("0.0.0.0"), self.port);

        let listener = TcpListener::bind(&addr).await?;
        info!("[WebRTC] Successfully bound to address {}", addr);
        info!("[WebRTC] WebRTC signaling server is ready and listening");
        info!("[WebRTC] WebSocket endpoint: ws://{}:{}/", addr.split(':').next().unwrap_or("0.0.0.0"), self.port);
        info!("[WebRTC] Supported signals: offer, answer, candidate");

        loop {
            match listener.accept().await {
                Ok((socket, peer_addr)) => {
                    info!("[WebRTC] New WebSocket connection from {}", peer_addr);
                    let manager = self.stream_manager.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_websocket(socket, manager).await {
                            error!("[WebRTC] Connection error from {}: {}", peer_addr, e);
                        }
                    });
                }
                Err(e) => {
                    error!("[WebRTC] Failed to accept connection: {}", e);
                    warn!("[WebRTC] Waiting for new connections...");
                }
            }
        }
    }

    async fn handle_websocket(socket: TcpStream, manager: Arc<StreamManager>) -> Result<()> {
        let mut buffer = vec![0u8; 8192];
        let mut socket = socket;

        loop {
            let n = socket.read(&mut buffer).await?;
            if n == 0 {
                break;
            }

            // WebSocket frame parsing
            let data = &buffer[..n];
            if let Some(msg) = Self::parse_websocket_frame(data) {
                let response = Self::process_webrtc_signal(&msg, &manager).await?;
                if !response.is_empty() {
                    let frame = Self::make_websocket_frame(&response);
                    socket.write_all(&frame).await?;
                }
            }
        }

        Ok(())
    }

    fn parse_websocket_frame(data: &[u8]) -> Option<String> {
        if data.len() < 2 {
            return None;
        }

        let fin = (data[0] & 0x80) != 0;
        let opcode = data[0] & 0x0F;
        let mask = (data[1] & 0x80) != 0;
        let mut payload_len = (data[1] & 0x7F) as usize;

        if !fin || opcode != 0x01 {
            return None;
        }

        let mut pos = 2;
        if payload_len == 126 {
            if data.len() < 4 {
                return None;
            }
            payload_len = ((data[2] as usize) << 8) | (data[3] as usize);
            pos = 4;
        } else if payload_len == 127 {
            if data.len() < 10 {
                return None;
            }
            pos = 10;
        }

        let mut mask_bytes = [0u8; 4];
        if mask && data.len() >= pos + 4 {
            mask_bytes.copy_from_slice(&data[pos..pos + 4]);
            pos += 4;
        }

        if data.len() < pos + payload_len {
            return None;
        }

        let mut payload = data[pos..pos + payload_len].to_vec();
        if mask {
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask_bytes[i % 4];
            }
        }

        String::from_utf8(payload).ok()
    }

    fn make_websocket_frame(data: &str) -> Vec<u8> {
        let len = data.len();
        let mut frame = Vec::with_capacity(2 + len);

        frame.push(0x81); // FIN + text opcode

        if len < 126 {
            frame.push(len as u8);
        } else if len < 65536 {
            frame.push(126);
            frame.push((len >> 8) as u8);
            frame.push((len & 0xFF) as u8);
        } else {
            frame.push(127);
            for i in (0..8).rev() {
                frame.push((len >> (i * 8)) as u8);
            }
        }

        frame.extend_from_slice(data.as_bytes());
        frame
    }

    async fn process_webrtc_signal(msg: &str, manager: &StreamManager) -> Result<String> {
        info!("WebRTC signal: {}", msg);

        if msg.contains("\"type\":\"offer\"") {
            // Generate answer
            let answer = r#"{"type":"answer","sdp":"v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n"}"#;
            Ok(answer.to_string())
        } else if msg.contains("\"type\":\"candidate\"") {
            Ok(String::new())
        } else {
            Ok(String::new())
        }
    }
}

pub struct WebrtcTranscoder {
    stream_manager: Arc<StreamManager>,
}

impl WebrtcTranscoder {
    pub fn new(stream_manager: Arc<StreamManager>) -> Self {
        Self { stream_manager }
    }

    pub async fn transcode_to_webrtc(&self, stream_id: &str) -> Result<()> {
        info!("Transcoding stream {} to WebRTC", stream_id);
        Ok(())
    }
}
