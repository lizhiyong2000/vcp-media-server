use std::sync::Arc;

use tokio::{signal};

use vcp_media_common::log::logger;

mod common;
use vcp_media_common::Result;

use log::{self, info};


mod server;

use server::tcp_server::{TcpServer, ServerType};
use server::message_hub::MessageHub;

use crate::server::EventSender;

#[tokio::main]
async fn main() -> Result<()> {
    let guard = logger::setup_log();

    info!("setup main");

    let event_sender:Arc<Box<dyn EventSender>>= Arc::new(Box::new(MessageHub::new()));

    let es1 = event_sender.clone();
    let es2 = event_sender.clone();

    tokio::spawn(async{
        let rtsp_server = TcpServer::new(ServerType::RTSP, "0.0.0.0:8554".to_string(), es1);
        rtsp_server.start().await;
    }
    );

    tokio::spawn(async{
        let rtmp_server = TcpServer::new(ServerType::RTMP, "0.0.0.0:1935".to_string(), es2);
        rtmp_server.start().await;
    }
    );

    signal::ctrl_c().await?;
    drop(guard);
    Ok(())
}
