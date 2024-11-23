use std::net::SocketAddr;
use async_trait::async_trait;
use log::info;
use vcp_media_common::server::{NetworkServer, SessionError, TcpServerHandler};
use vcp_media_common::server::tcp_server::TcpServer;
use vcp_media_common::uuid::{RandomDigitCount, Uuid};
use vcp_media_rtmp::session::server_session::{RTMPServerSession, RtmpServerSessionHandler};
use crate::manager::message::StreamHubEventSender;


pub struct VcpRtmpServerSessionHandler {

    event_producer: StreamHubEventSender,
}



impl VcpRtmpServerSessionHandler{
    pub fn new(event_producer: StreamHubEventSender) -> Self {

        VcpRtmpServerSessionHandler{event_producer}
    }
}

impl RtmpServerSessionHandler for VcpRtmpServerSessionHandler {

}

pub struct RtmpServerHandler {
    hub_event_sender: StreamHubEventSender
}

impl RtmpServerHandler
{
    pub fn new(hub_event_sender: StreamHubEventSender) -> Self {
        Self {hub_event_sender}
    }
}

#[async_trait]
impl TcpServerHandler<RTMPServerSession> for crate::server::rtmp_server::RtmpServerHandler
{
    async fn on_create_session(&mut self, sock: tokio::net::TcpStream, remote: SocketAddr) -> Result<RTMPServerSession, SessionError> {
        // info!("Session {} created", session_id);
        let id = Uuid::new(RandomDigitCount::Zero).to_string();
        Ok(RTMPServerSession::new(id, sock, remote, Some(Box::new(VcpRtmpServerSessionHandler::new(self.hub_event_sender.clone())))))
    }

    async fn on_session_created(&mut self, session_id: String) -> Result<(), SessionError> {
        info!("Session {} created", session_id);
        Ok(())
    }
}




pub struct RtmpServer {
    tcp_server: TcpServer<RTMPServerSession>,
    // hub_event_sender: StreamHubEventSender,
}


impl crate::server::rtmp_server::RtmpServer {
    pub fn new(addr: String, hub_event_sender: StreamHubEventSender) -> Self {
        let server_handler = Box::new(
            crate::server::rtmp_server::RtmpServerHandler::new(hub_event_sender)
        );
        let rtmp_server: TcpServer<RTMPServerSession> = TcpServer::new(addr, Some(server_handler));

        let res = Self {
            tcp_server: rtmp_server,
            // hub_event_sender,
        };
        res
    }

    pub fn session_type(&self) -> String {
        self.tcp_server.session_type()
    }

    pub async fn start(&mut self) -> Result<(), SessionError> {
        self.tcp_server.start().await
    }
}
