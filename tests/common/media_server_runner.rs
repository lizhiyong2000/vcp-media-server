//! Embedded media server executable runner for integration tests.
//!
//! Starts the actual media server executable for end-to-end testing.

/// Ensures `cargo test` builds the server binary before running integration tests.
const _: &str = env!("CARGO_BIN_EXE_vcp-media-server");

use std::fs;
use std::net::TcpListener as StdTcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::sleep;

use super::http_client::{http_get, http_post_json, HttpResponse};
use super::timeouts;

/// Default server ports configuration.
#[derive(Debug, Clone)]
pub struct ServerPorts {
    pub rtmp: u16,
    pub rtsp: u16,
    pub http: u16,
    pub webrtc: u16,
}

impl Default for ServerPorts {
    fn default() -> Self {
        Self {
            rtmp: 1935,
            rtsp: 554,
            http: 8081,
            webrtc: 9080,
        }
    }
}

impl ServerPorts {
    /// Ask the OS for currently available listener ports for parallel tests.
    pub fn available() -> std::io::Result<Self> {
        let rtmp = bind_available_port()?;
        let rtsp = bind_available_port()?;
        let http = bind_available_port()?;
        let webrtc = bind_available_port()?;

        let ports = Self {
            rtmp: rtmp.local_addr()?.port(),
            rtsp: rtsp.local_addr()?.port(),
            http: http.local_addr()?.port(),
            webrtc: webrtc.local_addr()?.port(),
        };

        drop((rtmp, rtsp, http, webrtc));
        Ok(ports)
    }
}

/// A running instance of the media server executable.
pub struct MediaServer {
    ports: ServerPorts,
    child: Child,
    config_path: PathBuf,
    log_label: Option<String>,
}

impl MediaServer {
    /// Start the media server on default ports (requires ports to be free).
    pub fn start() -> std::io::Result<Self> {
        Self::start_with_ports_inner(ServerPorts::default(), None)
    }

    /// Start the media server on available ports selected by the OS.
    pub fn start_isolated() -> std::io::Result<Self> {
        Self::start_with_ports_inner(ServerPorts::available()?, None)
    }

    /// Start the media server on available ports and prefix runner logs with a stream id.
    pub fn start_isolated_for_stream(stream_id: &str) -> std::io::Result<Self> {
        Self::start_with_ports_inner(ServerPorts::available()?, Some(stream_id.to_string()))
    }

    /// Start the media server with specific ports via config file.
    pub fn start_with_ports(ports: ServerPorts) -> std::io::Result<Self> {
        Self::start_with_ports_inner(ports, None)
    }

    fn start_with_ports_inner(
        ports: ServerPorts,
        log_label: Option<String>,
    ) -> std::io::Result<Self> {
        let exe_path = find_server_executable()?;
        if !exe_path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "media server binary not found at {} — run `cargo build --bin vcp-media-server` first",
                    exe_path.display()
                ),
            ));
        }

        let config_path = create_test_config(&ports)?;

        let child = Command::new(&exe_path)
            .arg(&config_path)
            .env("VCP_CONFIG_PATH", &config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        // println!(
        //     "{}[MEDIA_SERVER_PORTS] started exe={} rtmp={} rtsp={} http={} webrtc={} config={}",
        //     log_prefix(log_label.as_deref()),
        //     exe_path.display(),
        //     ports.rtmp,
        //     ports.rtsp,
        //     ports.http,
        //     ports.webrtc,
        //     config_path.display()
        // );

        Ok(Self {
            ports,
            child,
            config_path,
            log_label,
        })
    }

    /// Wait for the server to be ready on RTMP/RTSP/HTTP ports.
    pub async fn wait_ready(&self) {
        let deadline = tokio::time::Instant::now() + timeouts::SERVER_START;
        wait_for_port("127.0.0.1", self.ports.rtmp, deadline).await;
        wait_for_port("127.0.0.1", self.ports.rtsp, deadline).await;
        wait_for_port("127.0.0.1", self.ports.http, deadline).await;
        println!(
            "{}[MEDIA_SERVER_PORTS] ready rtmp={} rtsp={} http={} webrtc={}",
            log_prefix(self.log_label.as_deref()),
            self.ports.rtmp,
            self.ports.rtsp,
            self.ports.http,
            self.ports.webrtc
        );
    }

    pub fn rtmp_url(&self, app: &str, stream_key: &str) -> String {
        format!(
            "rtmp://127.0.0.1:{}/{}/{}",
            self.ports.rtmp, app, stream_key
        )
    }

    pub fn rtsp_url(&self, stream_id: &str) -> String {
        format!("rtsp://127.0.0.1:{}/{}", self.ports.rtsp, stream_id)
    }

    pub fn http_flv_url(&self, stream_id: &str) -> String {
        format!("http://127.0.0.1:{}/flv/{}", self.ports.http, stream_id)
    }

    pub fn hls_url(&self, stream_id: &str) -> String {
        format!(
            "http://127.0.0.1:{}/hls/{}/live.m3u8",
            self.ports.http, stream_id
        )
    }

    pub fn http_host(&self) -> &'static str {
        "127.0.0.1"
    }

    pub fn http_port(&self) -> u16 {
        self.ports.http
    }

    /// POST JSON to an HTTP API path (e.g. `/api/pull/rtmp`).
    pub async fn post_json(&self, path: &str, json_body: &str) -> Result<HttpResponse, String> {
        http_post_json(self.http_host(), self.http_port(), path, json_body).await
    }

    /// GET an HTTP path on the media server.
    pub async fn get(&self, path: &str) -> Result<HttpResponse, String> {
        http_get(self.http_host(), self.http_port(), path).await
    }

    /// Start transcoding via HTTP API.
    pub async fn api_transcode_start(&self, json_body: &str) -> Result<HttpResponse, String> {
        self.post_json("/api/transcode/start", json_body).await
    }

    /// Stop transcoding via HTTP API.
    pub async fn api_transcode_stop(&self, json_body: &str) -> Result<HttpResponse, String> {
        self.post_json("/api/transcode/stop", json_body).await
    }

    /// Start RTMP pull via HTTP API.
    pub async fn api_rtmp_pull(
        &self,
        remote_url: &str,
        stream_id: &str,
    ) -> Result<HttpResponse, String> {
        let body = format!(r#"{{"url":"{remote_url}","stream_id":"{stream_id}"}}"#);
        self.post_json("/api/pull/rtmp", &body).await
    }

    /// Start RTSP pull via HTTP API.
    pub async fn api_rtsp_pull(
        &self,
        remote_url: &str,
        stream_id: &str,
    ) -> Result<HttpResponse, String> {
        let body = format!(r#"{{"url":"{remote_url}","stream_id":"{stream_id}"}}"#);
        self.post_json("/api/pull/rtsp", &body).await
    }

    /// Trigger HLS generation with a single playlist request (avoid repeated m3u8 polling).
    pub async fn trigger_hls(&self, stream_id: &str) -> Result<(), String> {
        let path = format!("/hls/{stream_id}/live.m3u8");
        tokio::time::timeout(Duration::from_secs(10), self.get(&path))
            .await
            .map_err(|_| format!("HLS trigger timed out for {stream_id}"))??;
        Ok(())
    }

    pub fn ports(&self) -> &ServerPorts {
        &self.ports
    }

    pub fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.config_path);
        println!(
            "{}Media server stopped",
            log_prefix(self.log_label.as_deref())
        );
    }
}

impl Drop for MediaServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn log_prefix(log_label: Option<&str>) -> String {
    log_label
        .map(|label| format!("[{label}] "))
        .unwrap_or_default()
}

async fn wait_for_port(host: &str, port: u16, deadline: tokio::time::Instant) {
    loop {
        if TcpStream::connect((host, port)).await.is_ok() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("Timed out waiting for server on {}:{}", host, port);
        }
        sleep(Duration::from_millis(50)).await;
    }
}

fn bind_available_port() -> std::io::Result<StdTcpListener> {
    StdTcpListener::bind(("0.0.0.0", 0))
}

fn create_test_config(ports: &ServerPorts) -> std::io::Result<PathBuf> {
    let config_content = format!(
        r#"# Test config for integration tests
[storage]
base_dir = "./saving"

[server.rtmp]
port = {}

[server.rtsp]
port = {}

[server.webrtc]
port = {}

[server.http]
port = {}

[server.hls]
enabled = true
segment_duration = 1
max_segments = 1
output_dir = "hls"

[server.http_flv]
enabled = true

[record]
enabled = false
output_dir = "recordings"

[analysis]
enabled = false

[snapshot]
enabled = false

[transcode]
enabled = true
ffmpeg_path = "ffmpeg"
max_workers = 4

[log]
level = "warn"
path = "./logs/media-server-test.log"
max_size_mb = 10
max_files = 3
"#,
        ports.rtmp, ports.rtsp, ports.webrtc, ports.http
    );

    let config_path = std::env::temp_dir().join(format!(
        "vcp_media_server_test_{}_{}.toml",
        std::process::id(),
        ports.rtmp
    ));
    fs::write(&config_path, config_content)?;
    Ok(config_path)
}

fn find_server_executable() -> std::io::Result<PathBuf> {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_vcp-media-server") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    for dir in ["target/debug", "target/release", "."] {
        for name in ["vcp-media-server", "media-server"] {
            let path = PathBuf::from(dir).join(name);
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    Ok(PathBuf::from("target/debug/vcp-media-server"))
}
