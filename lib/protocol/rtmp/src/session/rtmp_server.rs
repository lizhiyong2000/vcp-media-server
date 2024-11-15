
use std::net::SocketAddr;
use tokio::io::Error;
use tokio::net::TcpListener;

pub struct RtmpServer {
    address: String,
    gop_num: usize,
}

impl RtmpServer {
    pub fn new(
        address: String,
        gop_num: usize,
    ) -> Self {
        Self {
            address,
            gop_num,
        }
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        let socket_addr: &SocketAddr = &self.address.parse().unwrap();
        let listener = TcpListener::bind(socket_addr).await?;

        log::info!("Rtmp server listening on tcp://{}", socket_addr);
        loop {
            let (tcp_stream, _) = listener.accept().await?;
            //tcp_stream.set_keepalive(Some(Duration::from_secs(30)))?;

            // tokio::spawn(async move {
            //     if let Err(err) = session.run().await {
            //         log::info!(
            //             "session run error: session_type: {}, app_name: {}, stream_name: {}, err: {}",
            //             session.common.session_type,
            //             session.app_name,
            //             session.stream_name,
            //             err
            //         );
            //     }
            // });
        }
    }
}
