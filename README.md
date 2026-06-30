# vcp-media-server

Rust 实现的轻量级流媒体服务，以 `StreamManager` 为媒体中枢，支持多协议接入、跨协议分发与统一流管理。面向直播、监控、低延迟预览等场景。

## 方案架构

```
┌─────────────────────────────────────────────────────────────────┐
│  控制面：HTTP API · WebRTC 信令 · config.toml                    │
├─────────────────────────────────────────────────────────────────┤
│  接入：RTMP · RTSP · WebRTC · RTMP/RTSP Pull · GB28181（规划）    │
├─────────────────────────────────────────────────────────────────┤
│  中枢：StreamManager — stream_id 隔离 · 广播 · GOP 缓存           │
├─────────────────────────────────────────────────────────────────┤
│  处理：录制 · 转码 · 分析（规划）                                  │
├─────────────────────────────────────────────────────────────────┤
│  分发：RTMP · RTSP · HTTP-FLV · HLS · WebRTC · 文件回放（规划）  │
└─────────────────────────────────────────────────────────────────┘
```

**核心机制**

- 统一 `StreamManager`：多协议写入、多协议读出，按 `stream_id` 隔离
- H264 Annex B / RTP 解析，SPS/PPS 提取与 SDP fmtp 注入
- WebRTC 低延迟播放：跳至 live 边缘、帧合并、IDR 起播
- HLS 按需切片：首次请求 m3u8 时启动 MPEG-TS 分段

## 按协议

| 协议 | 端口 | 接入（发布） | 分发（播放） | 编码 | 状态 |
|------|------|--------------|--------------|------|------|
| RTMP | 1935 | publish | play | H264 | 已支持 |
| RTSP | 554 | ANNOUNCE + RECORD（TCP / UDP） | DESCRIBE + PLAY（TCP / UDP） | H264 | 已支持 |
| HTTP-FLV | 8081 | — | `GET /flv/<stream_id>` | H264 | 已支持 |
| HLS | 8081 | — | `GET /hls/<stream_id>/live.m3u8` | H264 | 已支持 |
| WebRTC | 9080 | WebSocket 信令 publish | WebSocket 信令 play | H264 | 已支持 |
| GB28181 | 5060（规划） | SIP 注册 · PS over RTP 接入 | 国标级联输出 | H264 / H265（规划） | 规划中 |

**地址示例**

| 协议 | 示例 |
|------|------|
| RTMP | `rtmp://127.0.0.1:1935/live/stream1` |
| RTSP | `rtsp://127.0.0.1:554/stream1` |
| HTTP-FLV | `http://127.0.0.1:8081/flv/stream1` |
| HLS | `http://127.0.0.1:8081/hls/stream1/live.m3u8` |
| WebRTC 测试页 | `http://127.0.0.1:8081/webrtc/webrtc-test.html` |
| HTTP API | `http://127.0.0.1:8081/api/streams` |

## 按功能

### 流接入

| 功能 | 说明 | 状态 |
|------|------|------|
| RTMP 推流 | ffmpeg / OBS 等客户端 publish | 已支持 |
| RTSP 推流 | ffmpeg ANNOUNCE + RECORD，TCP / UDP | 已支持 |
| WebRTC 推流 | 浏览器经 WebSocket 信令发布 | 已支持 |
| RTMP 拉流 | HTTP API 从远端 RTMP 拉取并本地 relay | 已支持 |
| RTSP 拉流 | HTTP API 从远端 RTSP 拉取，支持 `?transport=udp` | 已支持 |
| RTSP 推流转发 | HTTP API 向远端 RTSP 地址推流 | 已支持 |
| GB28181 接入 | SIP 注册、目录、实时点播，PS 解复用入 Hub | 规划中 |

### 流分发

| 功能 | 说明 | 状态 |
|------|------|------|
| 跨协议播放 | 任一路接入，RTMP / RTSP / FLV / HLS / WebRTC 均可播放同一 `stream_id` | 已支持 |
| RTSP UDP 传输 | 推流、播放、拉流均支持 UDP RTP | 已支持 |
| WebRTC 多路播放 | 同一 `stream_id` 多浏览器独立 relay | 已支持 |
| 录制文件回放 | fMP4 / TS 归档，按时间段查询与下载 | 规划中 |
| GB28181 级联输出 | 向上级平台回传 PS over RTP | 规划中 |

### 编解码

| 功能 | 说明 | 状态 |
|------|------|------|
| H264 透传 | RTMP / RTSP / FLV / HLS / WebRTC 全链路 | 已支持 |
| AAC 音频 | RTMP / RTSP / FLV / HLS 伴音 | 已支持 |
| H265 / HEVC | VPS/SPS/PPS、RTSP/RTMP/HLS 全链路支持 | 规划中 |
| 视频转码 | 分辨率 / 码率 / 编码格式转换，输出衍生流（如 `live_sd`） | 规划中 |
| 滤镜 | 水印、帧率控制等 | 规划中 |

### 媒体处理

| 功能 | 说明 | 状态 |
|------|------|------|
| 直播 HLS 切片 | 按需生成 MPEG-TS，滑动窗口保留 | 已支持 |
| 视频录制 | 独立 DVR，关键帧对齐切片，长期保留 | 规划中 |
| 视频分析 | 码流指标、场景检测、插件化内容分析 | 规划中 |

### 管理与控制

| 功能 | 说明 | 状态 |
|------|------|------|
| 流列表 / 详情 | `GET /api/streams`、`GET /api/stream/<id>` | 已支持 |
| 流创建 / 删除 | `POST /api/streams`、`DELETE /api/stream/<id>` | 已支持 |
| 健康检查 | `GET /health` | 已支持 |
| 拉流 / 推流 API | `POST /api/rtmp/pull`、`POST /api/rtsp/pull`、`POST /api/rtsp/push` | 已支持 |
| 录制控制 API | 启停录制、查询录制列表与回放 | 规划中 |
| 转码 / 分析 API | 衍生流配置、分析任务与事件查询 | 规划中 |
| GB28181 API | 设备目录、点播控制 | 规划中 |

## 快速开始

```bash
cargo build
./target/debug/vcp-media-server
```

## 文档

| 文档 | 内容 |
|------|------|
| `docs/test-cases.md` | 全部测试用例（TC-01～TC-27，含完整命令） |
| `docs/architecture.md` | 架构规划（H265、录制、转码、分析、GB28181） |
| `docs/getting-started.md` | 编译、启动、端口、API 速查 |
| `docs/troubleshooting.md` | 故障排查 |
