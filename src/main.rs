use std::sync::Arc;

use tokio::{net::TcpSocket, signal};

mod utils;
use utils::logger;

mod common;
use common::Result;

use log::{self, info};


mod server;

use server::tcp_server::{TcpServer, ServerType};
use server::message_hub::MessageHub;

use crate::server::EventSender;

#[tokio::main]
async fn main() -> Result<()> {
    logger::setup_log();

    info!("setup main");

    let event_sender:Arc<Box<dyn EventSender>>= Arc::new(Box::new(MessageHub::new()));

    let es1 = event_sender.clone();
    let es2 = event_sender.clone();

    tokio::spawn(async{
        let rtsp_server = TcpServer::new(ServerType::RTSP, "0.0.0.0:9999".to_string(), es1);
        rtsp_server.start().await;
    }
    );

    tokio::spawn(async{
        let rtmp_server = TcpServer::new(ServerType::RTMP, "0.0.0.0:9998".to_string(), es2);
        rtmp_server.start().await;
    }
    );

    signal::ctrl_c().await?;
    Ok(())
}
