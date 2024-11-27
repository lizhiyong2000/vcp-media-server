use crate::server::http_server;
use crate::server::rtsp_server::RtspServer;
use log::{self, info};
use vcp_media_common::server::tcp_server::TcpServer;
use vcp_media_common::server::NetworkServer;
use vcp_media_rtmp::session::server_session::RtmpServerSession;
use crate::manager::stream_hub::StreamHub;
use crate::server::rtmp_server::RtmpServer;

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

        let mut stream_hub = StreamHub::new();

        // tokio::spawn(async {
            Self::start_http_service("0.0.0.0:3000".to_string()).await;
        // });

        // tokio::spawn(async{
            Self::start_rtsp_service("0.0.0.0:8554".to_string(), &mut stream_hub).await;
        // });

        // tokio::spawn(async {
            Self::start_rtmp_service("0.0.0.0:1935".to_string(), &mut stream_hub).await;
        // });

        tokio::spawn(async move {
            stream_hub.run().await;
            log::info!("stream hub end...");
        });
    }

    async fn start_http_service(addr: String) {
        let listener = tokio::net::TcpListener::bind(addr.clone()).await.unwrap();

        // info!("to start api service");

        tokio::spawn(async move {
            info!("HTTP server started listen at:{}", addr);
            // info!("to start api service");
            http_server::start_api_server(listener).await;

            info!("HTTP server end running.");
        });


    }

    async fn start_rtsp_service(addr: String, stream_hub_sender: &mut StreamHub) {
        let mut rtsp_server = RtspServer::new(addr, stream_hub_sender.get_sender());
        tokio::spawn(async move {

            let res = rtsp_server.start().await;
            match res {
                Ok(_) => info!("{} server end running.", rtsp_server.session_type()),
                Err(e) => info!("{} server error:{}", rtsp_server.session_type(), e)
            }
        });



    }

    async fn start_rtmp_service(addr: String, stream_hub_sender: &mut StreamHub) {

        let mut rtmp_server = RtmpServer::new(addr, stream_hub_sender.get_sender());
        tokio::spawn(async move {

            let res = rtmp_server.start().await;
            match res {
                Ok(_) => info!("{} server end running.", rtmp_server.session_type()),
                Err(e) => info!("{} server error:{}", rtmp_server.session_type(), e)
            }
        });


    }
}

struct Config {}

impl Config {
    pub fn load(config_path: &str) -> Self {
        let cfg = Config {};

        return cfg;
    }
}
