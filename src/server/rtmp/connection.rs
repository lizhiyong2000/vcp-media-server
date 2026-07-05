use anyhow::{anyhow, Result};
use bytes::{Buf, BytesMut};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{info, warn};

use super::chunk::{self, ChunkAssembler, ChunkHeader, RtmpMessage};
use crate::server::socket_transport::SocketReader;

pub struct RtmpConnection {
    reader: SocketReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    chunk_states: HashMap<u32, ChunkAssembler>,
    chunk_message_headers: HashMap<u32, ChunkHeader>,
    chunk_size: usize,
    buf: BytesMut,
}

impl RtmpConnection {
    pub fn new(stream: TcpStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader: SocketReader::new(reader),
            writer,
            chunk_states: HashMap::new(),
            chunk_message_headers: HashMap::new(),
            chunk_size: 128,
            buf: BytesMut::with_capacity(8192),
        }
    }

    pub async fn server_handshake(&mut self, peer_addr: std::net::SocketAddr) -> Result<bool> {
        if self.buf.len() < 1537 {
            return Ok(false);
        }

        if self.buf[0] != 0x03 {
            warn!(
                "[RTMP] [{}] Invalid RTMP version: 0x{:02X}",
                peer_addr, self.buf[0]
            );
            self.buf.clear();
            return Ok(false);
        }

        info!(
            "[RTMP] [{}] Received RTMP version 0x{:02X}, starting handshake",
            peer_addr, self.buf[0]
        );

        let mut response = vec![0x03];
        response.extend_from_slice(&[0u8; 1536]);
        response.extend_from_slice(&self.buf[1..1537]);

        self.writer.write_all(&response).await?;
        info!(
            "[RTMP] [{}] Sent handshake response ({} bytes)",
            peer_addr,
            response.len()
        );

        self.buf.advance(1537);

        if self.buf.len() >= 1536 {
            self.buf.advance(1536);
            info!("[RTMP] [{}] Handshake completed", peer_addr);
            return Ok(true);
        }

        Ok(false)
    }

    pub async fn client_handshake(&mut self) -> Result<()> {
        let mut c0c1 = vec![0x03u8];
        c0c1.extend(vec![0u8; 1536]);
        self.writer.write_all(&c0c1).await?;

        let mut response = vec![0u8; 3073];
        let mut remaining = response.len();
        let mut offset = 0;
        while remaining > 0 {
            let len = self.reader.read(&mut response[offset..]).await?;
            if len == 0 {
                return Err(anyhow!("Connection closed during handshake"));
            }
            offset += len;
            remaining -= len;
        }

        if response[0] != 0x03 {
            return Err(anyhow!("Unexpected RTMP version: 0x{:02X}", response[0]));
        }

        self.writer.write_all(&response[1..1537]).await?;
        Ok(())
    }

    pub async fn read(&mut self) -> Result<usize> {
        let len = self.reader.read_into(&mut self.buf).await?;
        Ok(len)
    }

    pub async fn read_timeout(&mut self, timeout: std::time::Duration) -> Result<usize> {
        let mut read_buf = [0u8; 8192];
        let len = tokio::time::timeout(timeout, self.reader.read(&mut read_buf)).await??;
        if len > 0 {
            self.buf.extend_from_slice(&read_buf[..len]);
        }
        Ok(len)
    }

    pub fn parse_chunks(&mut self) -> Vec<RtmpMessage> {
        let mut messages = Vec::new();
        while let Some(msg) = chunk::parse_chunks(
            &mut self.buf,
            &mut self.chunk_states,
            &mut self.chunk_message_headers,
            self.chunk_size,
        ) {
            messages.push(msg);
        }
        messages
    }

    pub async fn send_message(&mut self, msg_type: u8, msg_stream_id: u32, payload: &[u8]) -> Result<()> {
        self.send_message_with_timestamp(msg_type, 0, msg_stream_id, payload, chunk::CSID_COMMAND).await
    }

    pub async fn send_message_with_timestamp(
        &mut self,
        msg_type: u8,
        timestamp: u32,
        msg_stream_id: u32,
        payload: &[u8],
        chunk_stream_id: u32,
    ) -> Result<()> {
        let message = chunk::encode_message(msg_type, timestamp, msg_stream_id, payload, self.chunk_size, chunk_stream_id);
        self.writer.write_all(&message).await?;
        self.writer.flush().await?;
        Ok(())
    }

    pub fn handle_control_message(&mut self, payload: &[u8], msg_type: u8) -> Option<usize> {
        match msg_type {
            0x01 if payload.len() >= 4 => {
                let new_size = ((payload[0] as usize) << 24)
                    | ((payload[1] as usize) << 16)
                    | ((payload[2] as usize) << 8)
                    | payload[3] as usize;
                self.chunk_size = new_size;
                Some(new_size)
            }
            _ => None,
        }
    }

    pub fn set_chunk_size(&mut self, size: usize) {
        self.chunk_size = size;
    }

    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    pub fn buf_len(&self) -> usize {
        self.buf.len()
    }

    pub fn into_inner(self) -> (tokio::net::tcp::OwnedReadHalf, tokio::net::tcp::OwnedWriteHalf) {
        (self.reader.into_inner(), self.writer)
    }

    pub fn writer_clone(&self) -> tokio::net::tcp::OwnedWriteHalf {
        panic!("Cannot clone OwnedWriteHalf");
    }
}

pub enum RtmpHandshakeState {
    None,
    ServerWaitingC0C1,
    ServerWaitingC2,
    ClientWaitingS0S1S2,
    Completed,
}

pub struct RtmpHandshake {
    state: RtmpHandshakeState,
    peer_addr: std::net::SocketAddr,
}

impl RtmpHandshake {
    pub fn server(peer_addr: std::net::SocketAddr) -> Self {
        Self {
            state: RtmpHandshakeState::ServerWaitingC0C1,
            peer_addr,
        }
    }

    pub fn client() -> Self {
        Self {
            state: RtmpHandshakeState::ClientWaitingS0S1S2,
            peer_addr: "0.0.0.0:0".parse().unwrap(),
        }
    }

    pub fn is_completed(&self) -> bool {
        matches!(self.state, RtmpHandshakeState::Completed)
    }

    pub async fn server_handle_data(&mut self, buf: &mut BytesMut, writer: &mut tokio::net::tcp::OwnedWriteHalf) -> Result<bool> {
        match self.state {
            RtmpHandshakeState::ServerWaitingC0C1 => {
                if buf.len() < 1537 {
                    return Ok(false);
                }

                if buf[0] != 0x03 {
                    warn!(
                        "[RTMP] [{}] Invalid RTMP version: 0x{:02X}",
                        self.peer_addr, buf[0]
                    );
                    buf.clear();
                    return Ok(false);
                }

                info!(
                    "[RTMP] [{}] Received RTMP version 0x{:02X}, starting handshake",
                    self.peer_addr, buf[0]
                );

                let mut response = vec![0x03];
                response.extend_from_slice(&[0u8; 1536]);
                response.extend_from_slice(&buf[1..1537]);

                writer.write_all(&response).await?;
                info!(
                    "[RTMP] [{}] Sent handshake response ({} bytes)",
                    self.peer_addr,
                    response.len()
                );

                buf.advance(1537);
                self.state = RtmpHandshakeState::ServerWaitingC2;
                Ok(false)
            }
            RtmpHandshakeState::ServerWaitingC2 => {
                if buf.len() >= 1536 {
                    buf.advance(1536);
                    info!("[RTMP] [{}] Handshake completed", self.peer_addr);
                    self.state = RtmpHandshakeState::Completed;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            _ => Ok(true),
        }
    }

    pub async fn client_perform(&mut self, stream: &mut TcpStream) -> Result<()> {
        match self.state {
            RtmpHandshakeState::ClientWaitingS0S1S2 => {
                let mut c0c1 = vec![0x03u8];
                c0c1.extend(vec![0u8; 1536]);
                stream.write_all(&c0c1).await?;

                let mut response = vec![0u8; 3073];
                stream.read_exact(&mut response).await?;

                if response[0] != 0x03 {
                    return Err(anyhow!("Unexpected RTMP version: 0x{:02X}", response[0]));
                }

                stream.write_all(&response[1..1537]).await?;
                self.state = RtmpHandshakeState::Completed;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}