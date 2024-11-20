mod common;
mod server;
mod manager;
mod transmitter;

use vcp_media_common::log::logger;
use vcp_media_common::server::tcp_server::TcpServer;
use vcp_media_common::server::NetworkServer;
use vcp_media_common::Result;
use vcp_media_rtmp::session::server_session::RTMPServerSession;
use vcp_media_rtsp::session::server_session::RTSPServerSession;

use crate::manager::service::ServiceManager;
use log::{self, info};
use manager::stream_hub::StreamHub;
use std::sync::Arc;
use tokio::signal;

#[tokio::main]
async fn main() -> Result<()> {
    let guard = logger::setup_log();

    info!("setup main");
    //
    // let event_sender: Arc<Box<dyn EventSender>> = Arc::new(Box::new(MessageHub::new()));
    //
    // let es1 = event_sender.clone();
    // let es2 = event_sender.clone();



    let mut manager = ServiceManager::new("./config.toml");
    manager.start_service().await;


    signal::ctrl_c().await?;
    drop(guard);
    Ok(())
}
