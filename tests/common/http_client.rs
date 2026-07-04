//! Minimal async HTTP client for integration tests.

use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::sleep;

/// Parsed HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// Send HTTP GET and return status + body.
pub async fn http_get(host: &str, port: u16, path: &str) -> Result<HttpResponse, String> {
    tokio::time::timeout(Duration::from_secs(10), http_get_inner(host, port, path))
        .await
        .map_err(|_| format!("HTTP GET timed out: {path}"))?
}

async fn http_get_inner(host: &str, port: u16, path: &str) -> Result<HttpResponse, String> {
    let mut stream = TcpStream::connect((host, port))
        .await
        .map_err(|e| format!("connect {host}:{port}: {e}"))?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|e| e.to_string())?;

    parse_http_response(&raw)
}

/// Send HTTP POST with JSON body and return status + body.
pub async fn http_post_json(
    host: &str,
    port: u16,
    path: &str,
    json_body: &str,
) -> Result<HttpResponse, String> {
    tokio::time::timeout(
        Duration::from_secs(10),
        http_post_json_inner(host, port, path, json_body),
    )
    .await
    .map_err(|_| format!("HTTP POST timed out: {path}"))?
}

async fn http_post_json_inner(
    host: &str,
    port: u16,
    path: &str,
    json_body: &str,
) -> Result<HttpResponse, String> {
    let mut stream = TcpStream::connect((host, port))
        .await
        .map_err(|e| format!("connect {host}:{port}: {e}"))?;

    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        json_body.len(),
        json_body
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|e| e.to_string())?;

    parse_http_response(&raw)
}

/// Poll GET until HTTP 200 or timeout.
pub async fn wait_for_http_ok(
    host: &str,
    port: u16,
    path: &str,
    timeout: Duration,
) -> Result<HttpResponse, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match http_get(host, port, path).await {
            Ok(resp) if resp.status == 200 => return Ok(resp),
            Ok(resp) => {
                if Instant::now() >= deadline {
                    return Err(format!("HTTP {} not ready: status {}", path, resp.status));
                }
            }
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(e);
                }
            }
        }
        sleep(Duration::from_millis(200)).await;
    }
}

fn parse_http_response(raw: &[u8]) -> Result<HttpResponse, String> {
    let text = String::from_utf8_lossy(raw);
    let (header, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "invalid HTTP response".to_string())?;

    let status_line = header.lines().next().ok_or("missing status line")?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("invalid status line: {status_line}"))?;

    Ok(HttpResponse {
        status,
        body: body.to_string(),
    })
}
