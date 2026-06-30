# vcp-media-server

Rust 实现的轻量级流媒体服务，支持 RTMP / RTSP / HTTP-FLV / HLS / WebRTC 多协议接入与分发。

## 工程介绍

`vcp-media-server` 面向直播、监控、低延迟预览等场景，提供统一的流管理与多协议互转能力。外部推流客户端（ffmpeg、OBS 等）可将音视频推入服务，观众可通过 RTMP、RTSP、HTTP-FLV、HLS 或浏览器 WebRTC 播放同一 `stream_id`。

服务通过 HTTP API 管理流、触发 RTSP/RTMP 拉流与 RTSP 推流转发，并内置 WebRTC 测试页便于联调。

## 方案架构

```
                    ┌─────────────────────────────────────┐
  推流 / 拉流源      │         StreamManager (Hub)          │
  ───────────────►  │  广播 · SPS/PPS · 发布状态 · 订阅   │
  RTMP publish      │                                     │
  RTSP ANNOUNCE     └──────────┬──────────────────────────┘
  RTSP/RTMP pull               │
  WebRTC publish               ▼
                    ┌─────────────────────────────────────┐
  播放 / 分发        │  RTMP play │ RTSP PLAY │ FLV │ HLS  │
                    │  WebRTC play (H264 relay)            │
                    └─────────────────────────────────────┘
```

**核心机制**

- 统一 `StreamManager`：多协议写入、多协议读出，按 `stream_id` 隔离
- H264 Annex B / RTP 解析，SPS/PPS 提取与 SDP fmtp 注入
- WebRTC 低延迟播放：跳至 live 边缘、帧合并、IDR 起播
- HLS 按需切片：首次请求 m3u8 时启动 MPEG-TS 分段

**默认端口**（见 `config.toml`）

| 协议 | 端口 | 示例 |
|------|------|------|
| RTMP | 1935 | `rtmp://127.0.0.1:1935/live/stream1` |
| RTSP | 554 | `rtsp://127.0.0.1:554/stream1` |
| HTTP | 8081 | `http://127.0.0.1:8081/api/streams` |
| WebRTC 信令 | 9080 | `ws://127.0.0.1:9080/` |

## 已支持功能

| 能力 | 说明 |
|------|------|
| RTMP 推流 / 播放 | ffmpeg 等客户端 publish / play |
| RTMP 拉流转发 | HTTP API 从远端 RTMP 拉流并本地 relay |
| RTSP 推流 / 播放 | ANNOUNCE+RECORD 推流，DESCRIBE+PLAY 播放 |
| RTSP TCP / UDP 传输 | 推流、播放、拉流均支持 `transport=udp` |
| RTSP 拉流 / 推流 | HTTP API 从远端拉流或向远端推流 |
| HTTP-FLV | `GET /flv/<stream_id>`，需先推流入库 |
| HLS | `GET /hls/<stream_id>/live.m3u8`，按需 MPEG-TS 切片 |
| WebRTC 推流 / 播放 | WebSocket 信令 + 浏览器 H264 |
| WebRTC 跨协议播放 | RTMP / RTSP 推入 → 浏览器 WebRTC 播放 |
| 流管理 HTTP API | 创建/查询流、拉流/推流控制 |
| 内置 WebRTC 测试页 | `http://127.0.0.1:8081/webrtc/webrtc-test.html` |

## 文档

- `docs/test-cases.md` — 全部测试用例（TC-01～TC-27，含完整命令）
- `docs/getting-started.md` — 编译、启动、端口
- `docs/troubleshooting.md` — 故障排查

```bash
cargo build
./target/debug/vcp-media-server
```
