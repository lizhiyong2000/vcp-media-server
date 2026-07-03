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
- H265 / HEVC Annex B / RTP 解析，VPS/SPS/PPS 提取与跨协议参数集透传
- WebRTC 低延迟播放：跳至 live 边缘、帧合并、IDR 起播
- HLS 按需切片：首次请求 m3u8 时启动 MPEG-TS 分段

## 按协议

| 协议 | 端口 | 接入（发布） | 分发（播放） | 编码 | 状态 |
|------|------|--------------|--------------|------|------|
| RTMP | 1935 | publish | play | H264 / H265 | 已支持 |
| RTSP | 554 | ANNOUNCE + RECORD（TCP / UDP） | DESCRIBE + PLAY（TCP / UDP） | H264 / H265 | 已支持 |
| HTTP-FLV | 8081 | — | `GET /flv/<stream_id>` | H264 / H265 | 已支持 |
| HLS | 8081 | — | `GET /hls/<stream_id>/live.m3u8` | H264 / H265 | 已支持 |
| WebRTC | 9080 | WebSocket 信令 publish | WebSocket 信令 play | H264 / H265 | 已支持 |
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
| WebRTC 推流 | 浏览器经 WebSocket 信令发布，测试页可选择 H264 或 H265/HEVC | 已支持 |
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
| H265 / HEVC 透传 | RTMP / RTSP / FLV / HLS / WebRTC 全链路，支持 VPS/SPS/PPS 参数集传播 | 已支持 |
| AAC 音频 | RTMP / RTSP / FLV / HLS 伴音 | 已支持 |
| Opus 音频 | WebRTC 推流音频接入 | 已支持 |
| 视频转码 | 分辨率 / 码率 / 编码格式转换，输出衍生流（如 `live_sd`） | 规划中 |
| 滤镜 | 水印、帧率控制等 | 规划中 |

**H265 / HEVC 注意事项**

- RTMP H265 使用 Enhanced RTMP HEVC 标签；ffmpeg / ffplay 推拉 H265 RTMP 时通常需要 `-rtmp_enhanced_codecs hvc1`。
- RTSP H265 同时识别 `H265/90000` 和 `HEVC/90000` SDP 表达。
- HLS H265 使用 MPEG-TS HEVC stream type `0x24`，关键帧会携带 VPS/SPS/PPS 以保证独立解码。
- WebRTC H265 推流/播放依赖浏览器和系统能力；测试页会检测 `video/H265` 或 `video/HEVC` codec capability，不支持时会提示失败。

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
| 运行指标 | `GET /api/metrics`、`GET /api/metrics/<id>` — 关键帧请求、首帧耗时、播放器数、HLS 切片、ring/RTP gap | 已支持 |
| 拉流 / 推流 API | `POST /api/rtmp/pull`、`POST /api/rtsp/pull`、`POST /api/rtsp/push` | 已支持 |
| 录制控制 API | 启停录制、查询录制列表与回放 | 规划中 |
| 转码 / 分析 API | 衍生流配置、分析任务与事件查询 | 规划中 |
| GB28181 API | 设备目录、点播控制 | 规划中 |

## 快速开始

```bash
cargo build
./target/debug/vcp-media-server
```

**单元测试：**

```bash
cargo test
```

**集成测试（跨协议回归）：**

```bash
# 运行所有集成测试
cargo test --test integration_tests

# 运行特定测试模块
cargo test --test integration_tests h264
cargo test --test integration_tests h265

# 运行跨协议矩阵测试
cargo test --test integration_tests codec_matrix
```

集成测试覆盖：

| 测试维度 | 覆盖范围 |
|----------|----------|
| 视频编码 | H264, H265 |
| 发布方式 | RTMP 推流模拟, RTSP 发布模拟, API Push |
| 播放方式 | RTMP, RTSP, HTTP-FLV, HLS |
| 跨协议 | 任一发布方式 → 所有播放方式 |

WebRTC 推流/播放依赖浏览器和系统 codec 能力，通过 `webrtc/webrtc-test.html` 做浏览器端验证。

## 待办清单

### P1 功能补齐

- [ ] GB28181 接入与级联：仍是完整规划项。
- [ ] 转码子系统：分辨率、码率、帧率、编码格式转换，目前仍是规划项。
- [ ] 视频分析扩展：当前主要有 L1 metrics 和外部命令人脸检测；后续可补内置检测、目标检测、行为检测、Webhook/消息队列输出。
- [x] 快照支持 H265：快照接口已支持 H264/H265，H265 会补 VPS/SPS/PPS 并使用 ffmpeg `hevc` raw demuxer 生成 JPEG。
- [ ] 分析事件持久化：当前事件是内存限量保存，服务重启会丢失；可按需要落库或写文件索引。
- [ ] 录制文件回放/下载/按时间查询 API：录制已有基础能力，但 README 中的“录制文件回放”仍是规划项。
- [ ] WebRTC H265 兼容兜底：浏览器 H265 支持有限，后续应加 H265→H264 转码衍生流，作为 WebRTC 播放兜底。

### P2 稳定性与工程化

- [x] 增加跨协议自动化回归：已新增 `scripts/cross_protocol_regression.sh`，覆盖 H264/H265 × RTMP/RTSP/API 拉流发布 × RTMP/RTSP/HTTP-FLV/HLS 播放；WebRTC 由测试页做浏览器能力验证。
- [ ] 增加延迟回归测试：利用 WebRTC 页面测试信号读秒，固定采样 RTMP/FLV/HLS/WebRTC 首帧和播放延迟。
- [x] 监控与指标 API：把关键帧请求、首帧耗时、播放器连接数、HLS 切片状态、丢包/序列 gap 等指标结构化暴露出来。
- [ ] 清理 `cargo test` 中大量 `dead_code` 警告，降低后续排查噪音。
- [ ] HLS 低延迟能力继续优化：当前已修复 H265 B 帧 PTS/DTS、音频对齐和 live window，但还可以评估 LL-HLS、part segment 或更细的播放器参数文档。

## 文档

| 文档 | 内容 |
|------|------|
| `docs/test-cases.md` | 测试用例（按编码 × 发布方式，播放覆盖全格式） |
| `docs/architecture.md` | 架构规划（H265、录制、转码、分析、GB28181） |
| `docs/getting-started.md` | 编译、启动、端口、API 速查 |
| `docs/troubleshooting.md` | 故障排查 |
