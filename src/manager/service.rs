use crate::server::http_server;
use async_trait::async_trait;
use log::{self, info};
use vcp_media_common::server::tcp_server::TcpServer;
use vcp_media_common::server::NetworkServer;
use vcp_media_rtmp::session::server_session::RTMPServerSession;
use vcp_media_rtsp::session::server_session::RTSPServerSession;
use crate::server::rtsp_server::RtspServer;

pub struct ServiceManager {
    config: Config,
}

unsafe impl Send for ServiceManager {}
unsafe impl Sync for ServiceManager {}
// #[async_trait]
impl ServiceManager {
    pub fn new(config_path: &str) -> Self {
        let cfg = Config::load(config_path);

        return ServiceManager { config: cfg };
    }

    pub async fn start_service(&mut self) {
        tokio::spawn(async {
            Self::start_http_service("0.0.0.0:3000".to_string()).await;
        });

        tokio::spawn(async {
            Self::start_rtsp_service("0.0.0.0:8554".to_string()).await;
        });

        tokio::spawn(async {
            Self::start_rtmp_service("0.0.0.0:1935".to_string()).await;
        });

    }

    async fn start_http_service(addr: String) {
        let listener = tokio::net::TcpListener::bind(addr.clone()).await.unwrap();

        // info!("to start api service");

        info!("HTTP server started listen at:{}", addr);

        // info!("to start api service");
        http_server::start_api_server(listener).await;

        info!("HTTP server end running.");
    }

    async fn start_rtsp_service(addr: String) {
        let mut rtsp_server = RtspServer::new(addr);
        let res = rtsp_server.start().await;
        match res {
            Ok(_) => info!("{} server end running.", rtsp_server.session_type()),
            Err(e) => info!("{} server error:{}", rtsp_server.session_type(), e)
        }
    }

    async fn start_rtmp_service(addr: String) {
        let mut rtmp_server: TcpServer<RTMPServerSession> = TcpServer::new(addr, None);
        let res = rtmp_server.start().await;
        match res {
            Ok(_) => info!("{} server end running.", rtmp_server.session_type()),
            Err(e) => info!("{} server error:{}", rtmp_server.session_type(), e)
        }
    }
}

struct Config {}

impl Config {
    pub fn load(config_path: &str) -> Self {
        let cfg = Config {};

        return cfg;
    }
}
