mod common;
mod server;

use crate::server::message_hub::MessageHub;
use crate::server::EventSender;
use vcp_media_common::log::logger;
use vcp_media_common::server::tcp_server::TcpServer;
use vcp_media_common::server::NetworkServer;
use vcp_media_common::Result;
use vcp_media_rtmp::session::rtmp_session::RTMPServerSession;
use vcp_media_rtsp::session::server_session::RTSPServerSession;

use log::{self, info};
use std::sync::Arc;
use tokio::signal;


#[tokio::main]
async fn main() -> Result<()> {
    let guard = logger::setup_log();

    info!("setup main");

    let event_sender: Arc<Box<dyn EventSender>> = Arc::new(Box::new(MessageHub::new()));

    let es1 = event_sender.clone();
    let es2 = event_sender.clone();

    tokio::spawn(
        async {
            let mut rtsp_server: TcpServer<RTSPServerSession> = TcpServer::new("0.0.0.0:8554".to_string());
            if let Ok(res) = rtsp_server.start().await{
                info!("{} server end running.", rtsp_server.session_type());
            }else {
                info!("{} server failed to run!", rtsp_server.session_type());
            }
        }
    );

    tokio::spawn(
        async {
            let mut rtmp_server: TcpServer<RTMPServerSession> = TcpServer::new("0.0.0.0:8554".to_string());
            if let Ok(res) = rtmp_server.start().await{
                info!("{} server end running.", rtmp_server.session_type());
            }else {
                info!("{} server failed to run!", rtmp_server.session_type());
            }
        }
    );


    signal::ctrl_c().await?;
    drop(guard);
    Ok(())
}
