use std::net::SocketAddr;
use async_trait::async_trait;
use tokio::net::TcpStream;
use vcp_media_common::server::{tcp_server, NetworkSession, TcpSession};

pub struct RTMPServerSession{
    pub id: String,
}

#[async_trait]
impl NetworkSession for RTMPServerSession {
    fn id(&self) -> String {
        todo!()
    }

    fn session_type() -> String {
        return "RTMP".to_string()
    }

    async fn run(&mut self) {
        todo!()
    }
}

#[async_trait]
impl TcpSession for RTMPServerSession{

    fn from_tcp_socket(sock: TcpStream, remote: SocketAddr) -> Self {
        todo!()
    }
}