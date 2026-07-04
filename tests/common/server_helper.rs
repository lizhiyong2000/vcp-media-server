//! Embedded media server helpers for integration tests.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use vcp_media_server::core::StreamManager;
use vcp_media_server::server::rtmp::RtmpServer;

use super::ports;
use super::timeouts;

/// In-process RTMP server bound to an ephemeral port for integration tests.
pub struct TestRtmpServer {
    port: u16,
    manager: Arc<StreamManager>,
    handle: JoinHandle<()>,
}

impl TestRtmpServer {
    pub async fn start() -> Self {
        let port = ports::random_test_port();
        let manager = Arc::new(StreamManager::new());
        let server = RtmpServer::new(manager.clone(), port, None);
        let handle = tokio::spawn(async move {
            let _ = server.start().await;
        });

        wait_for_tcp("127.0.0.1", port, timeouts::SERVER_START).await;

        Self {
            port,
            manager,
            handle,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn manager(&self) -> Arc<StreamManager> {
        self.manager.clone()
    }

    pub fn publish_url(&self, app: &str, stream_key: &str) -> String {
        format!("rtmp://127.0.0.1:{}/{}/{}", self.port, app, stream_key)
    }
}

impl Drop for TestRtmpServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn wait_for_tcp(host: &str, port: u16, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if TcpStream::connect((host, port)).await.is_ok() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("Timed out waiting for RTMP test server on {}:{}", host, port);
        }
        sleep(Duration::from_millis(50)).await;
    }
}
