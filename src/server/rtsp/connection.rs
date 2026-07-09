use anyhow::Result;
use bytes::BytesMut;
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tracing::{debug, info};

use super::common::{format_rtsp_message, RtspCommon};
use super::messages::{RtspRequest, RtspResponse};
use crate::server::socket_transport::{spawn_write_loop, ChannelSocketWriter, SocketReader};

pub struct RtspConnection {
    reader: SocketReader<tokio::net::tcp::OwnedReadHalf>,
    writer: ChannelSocketWriter,
    peer_addr: SocketAddr,
}

impl RtspConnection {
    pub fn new(socket: TcpStream) -> Self {
        let peer_addr = socket
            .peer_addr()
            .unwrap_or_else(|_| "127.0.0.1:0".parse().unwrap());
        let (reader, writer) = socket.into_split();
        let (write_tx, write_rx) = channel(100);

        tokio::spawn(async move {
            spawn_write_loop(writer, write_rx).await;
        });

        Self {
            reader: SocketReader::new(reader),
            writer: ChannelSocketWriter::new(write_tx),
            peer_addr,
        }
    }

    pub fn peer_addr(&self) -> &SocketAddr {
        &self.peer_addr
    }

    pub async fn read_request(&mut self) -> Result<Option<RtspRequest>> {
        let mut buffer = BytesMut::with_capacity(8192);
        let request_data = self.read_rtsp_message(&mut buffer).await?;

        if request_data.is_empty() {
            return Ok(None);
        }

        let request_str = String::from_utf8_lossy(&request_data).to_string();
        debug!(
            "[RTSP] [{}] Received request:\n{}",
            self.peer_addr,
            format_rtsp_message(&request_str)
        );

        Ok(RtspRequest::parse(&request_str))
    }

    pub async fn read_response(&mut self) -> Result<String> {
        let mut buffer = BytesMut::with_capacity(8192);
        let data = self.read_rtsp_message(&mut buffer).await?;
        Ok(String::from_utf8_lossy(&data).to_string())
    }

    pub async fn send_request(&mut self, request: &RtspRequest) -> Result<()> {
        let request_str = request.to_string();
        debug!(
            "[RTSP] [{}] Sending request:\n{}",
            self.peer_addr,
            format_rtsp_message(&request_str)
        );
        self.writer.write_all(request_str.as_bytes()).await
    }

    pub async fn send_response(&mut self, response: &RtspResponse) -> Result<()> {
        let response_str = response.to_string();
        debug!(
            "[RTSP] [{}] Sending response:\n{}",
            self.peer_addr,
            format_rtsp_message(&response_str)
        );
        self.writer.write_all(response_str.as_bytes()).await
    }

    pub async fn send_rtp_packet(&self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data).await
    }

    pub async fn send_interleaved(&self, data: &[u8], channel: u8) -> Result<()> {
        let interleaved = RtspCommon::wrap_interleaved(data, channel);
        self.writer.write_all(&interleaved).await
    }

    pub async fn read_rtsp_message(&mut self, buffer: &mut BytesMut) -> Result<BytesMut> {
        loop {
            let n = self.reader.read_into(buffer).await?;
            if n == 0 {
                info!("[RTSP] [{}] Connection closed by peer", self.peer_addr);
                return Ok(BytesMut::new());
            }

            if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                let content_length = self.parse_content_length(buffer, pos);
                let total_length = pos + 4 + content_length;

                if buffer.len() >= total_length {
                    let data = buffer.split_to(total_length);
                    return Ok(data);
                }
            }
        }
    }

    pub async fn read_interleaved_data(&mut self) -> Result<Option<(u8, BytesMut)>> {
        let mut buffer = BytesMut::with_capacity(8192);
        loop {
            let n = self.reader.read_into(&mut buffer).await?;
            if n == 0 {
                return Ok(None);
            }

            if buffer.starts_with(b"$") && buffer.len() >= 4 {
                let channel = buffer[1];
                let length = ((buffer[2] as usize) << 8) | (buffer[3] as usize);
                let packet_length = 4 + length;

                if buffer.len() >= packet_length {
                    let mut data = buffer.split_to(packet_length);
                    let rtp_data = data.split_off(4);
                    return Ok(Some((channel, rtp_data)));
                }
            }

            if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                let request_data = buffer.split_to(pos + 4);
                debug!(
                    "[RTSP] [{}] Got RTSP message instead of interleaved data, ignoring",
                    self.peer_addr
                );
                continue;
            }
        }
    }

    fn parse_content_length(&self, buf: &[u8], header_end: usize) -> usize {
        let header_str = String::from_utf8_lossy(&buf[..header_end]);
        header_str
            .lines()
            .find(|l| l.to_lowercase().starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1))
            .map(|s| s.trim().parse::<usize>().unwrap_or(0))
            .unwrap_or(0)
    }

    pub fn writer(&self) -> &ChannelSocketWriter {
        &self.writer
    }

    pub async fn read_into(&mut self, buffer: &mut BytesMut) -> Result<usize> {
        self.reader.read_into(buffer).await
    }

    pub async fn close(&mut self) {
        drop(self.writer.sender().clone());
    }
}
