# 架构规划

本文档描述 vcp-media-server 的**现状架构**与**演进规划**，覆盖 H.265 编码、视频录制、视频转码、视频分析、GB28181 接入五大能力。供后续迭代设计与评审使用。

---

## 1. 现状架构

### 1.1 总体分层

当前实现为 **Hub-and-Spoke（中心广播）** 模型：各协议作为接入/分发适配器，共享 `StreamManager` 作为媒体中枢。

```
┌──────────────────────────────────────────────────────────────────┐
│  控制面 (Control Plane)                                           │
│  HTTP API · config.toml · WebRTC WebSocket 信令                   │
├──────────────────────────────────────────────────────────────────┤
│  接入层 (Ingest Adapters)                                         │
│  RTMP publish · RTSP ANNOUNCE/RECORD · RTSP/RTMP Pull · WebRTC   │
├──────────────────────────────────────────────────────────────────┤
│  媒体中枢 (Media Hub)                                             │
│  StreamManager — stream_id 隔离 · broadcast 广播 · GOP 缓存       │
├──────────────────────────────────────────────────────────────────┤
│  分发层 (Egress Adapters)                                       │
│  RTMP play · RTSP PLAY · HTTP-FLV · HLS · WebRTC play            │
└──────────────────────────────────────────────────────────────────┘
```

**入口：** `src/main.rs` 加载配置后并行启动 RTMP、RTSP、WebRTC、HTTP 服务，共享 `Arc<StreamManager>`。

### 1.2 核心数据模型

| 类型 | 文件 | 职责 |
|------|------|------|
| `StreamManager` | `src/core/mod.rs` | 流注册、广播通道、`publish_frame` / `subscribe` |
| `Stream` | `src/core/mod.rs` | 元数据、tracks、SPS/PPS、GOP 缓冲、发布状态 |
| `MediaFrame` | `src/core/mod.rs` | 统一帧：`stream_id`、`track_id`、`timestamp`、`data`、`codec` |
| `CodecType` | `src/core/mod.rs` | H264 / H265 / AAC / Opus / G711（枚举已定义，实现以 H264 为主） |

**内部视频格式约定：** Annex B NALU（`00 00 00 01` + NAL），音频为 AAC ADTS 等原始负载。

**广播机制：** 每个 `stream_id` 对应一个 `tokio::sync::broadcast` 通道（容量 2048）。所有分发模块通过 `subscribe(stream_id)` 消费帧，互不阻塞热路径。

### 1.3 协议模块现状

| 模块 | 路径 | 接入 | 分发 | 编解码深度 |
|------|------|------|------|------------|
| RTMP | `src/rtmp/` | AVCC→Annex B | Annex B→AVCC | H264 完整；H265 仅识别 codec id |
| RTSP | `src/rtsp/` | RTP 解包 | RTP 打包 (FU-A) | H264 完整；H265 SDP 可解析，PLAY 未实现 |
| WebRTC | `src/webrtc/` | RTP depacketize | H264 sample 写出 | 仅 H264 |
| HLS | `src/hls/` | 订阅 hub | MPEG-TS 切片 | H264；PMT 硬编码 0x1B |
| HTTP-FLV | `src/http_flv/` | 订阅 hub | FLV tag | H264；H265 走 AVC 路径（不正确） |
| HTTP API | `src/http/` | 流管理、拉流/推流 API | HLS/FLV 路由 | 无编解码 |

### 1.4 配置与缺口

`config.toml` 中已有 `[hub]`、`streams.transcode`、`streams.outputs`、`streams.filters` 等**占位配置**，但 `src/core/config.rs` 尚未解析，运行时未生效。

当前所有 relay 均为 **码流透传（passthrough）**，无解码/重编码、无持久化录制（HLS 切片仅为直播滑动窗口）、无 GB28181 / SIP 相关代码。

### 1.5 可复用设计模式

1. **新能力优先挂接 `StreamManager` 广播**，避免在各协议模块重复逻辑。
2. **`MediaFrame` 作为跨模块帧契约**，扩展编解码参数集（VPS 等）而非引入第二套帧类型。
3. **`StreamSink` / `StreamSource` trait**（`src/core/protocol.rs`）已定义但未落地，可作为统一接入/分发抽象的目标接口。
4. **HTTP API 作为控制面扩展点**，录制/转码/GB28181 会话管理均由此暴露。

---

## 2. 目标架构

在保持 Hub 模型的前提下，引入 **处理平面（Processing Plane）** 与 **国标接入层**，形成四层结构：

```
┌─────────────────────────────────────────────────────────────────────┐
│  控制面                                                              │
│  HTTP API · GB28181 平台对接 API · config.toml                      │
├─────────────────────────────────────────────────────────────────────┤
│  接入层                                                              │
│  RTMP · RTSP · WebRTC · GB28181 (SIP+PS) · Pullers                  │
├─────────────────────────────────────────────────────────────────────┤
│  媒体中枢 StreamManager                                              │
│  主流 (source) · 衍生流 (derivative) · 参数集 · GOP · 元数据          │
├─────────────────────────────────────────────────────────────────────┤
│  处理平面 (新增)                                                     │
│  Recorder · Transcoder · Analyzer · Filter                          │
├─────────────────────────────────────────────────────────────────────┤
│  分发层                                                              │
│  RTMP · RTSP · FLV · HLS · WebRTC · GB28181 级联 · 文件回放          │
└─────────────────────────────────────────────────────────────────────┘
```

**衍生流（Derivative Stream）：** 转码、水印、降码率等处理结果发布为新的 `stream_id`（如 `live` → `live_sd`），仍走同一 `StreamManager`，分发层无感知。

---

## 3. H.265 编码支持

### 3.1 目标

- 全链路支持 HEVC 码流的接入、存储、转发与播放（在容器/协议允许的前提下）。
- 与现有 H264 路径对称，内部仍以 Annex B 为视频帧格式。
- WebRTC 播放侧视浏览器能力，必要时自动转码为 H264。

### 3.2 核心层改造

**`src/core/mod.rs`**

```rust
// Stream 扩展
pub struct Stream {
    // 现有字段 ...
    pub vps: Option<Vec<u8>>,   // HEVC Video Parameter Set
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

// 新增
pub fn merge_hevc_nalu_config(stream_id, vps, sps, pps);
pub fn get_hevc_parameter_sets(stream_id) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)>;
```

**`MediaFrame`：** 保持 `CodecType::H265`，NAL 解析与关键帧检测复用 `h264_util` 思路，新建 `src/core/hevc_util.rs`（或 `src/codec/hevc.rs`）。

### 3.3 各协议改造要点

| 协议 | 文件 | 工作项 |
|------|------|--------|
| RTMP | `src/rtmp/mod.rs`, `session.rs` | 解析/生成 HVCC sequence header；codec id `0x0C`；推流 ingest 标记 `CodecType::H265` |
| RTSP | `src/rtsp/common.rs`, `server_session.rs` | `H265RtpIngest`；HEVC RTP 打包（AP/FU）；SDP `h265` fmtp（`sprop-vps`/`sprop-sps`/`sprop-pps`） |
| HLS | `src/hls/ts_muxer.rs` | PMT stream type `0x24`；HEVC PES；关键帧前注入 VPS/SPS/PPS |
| HTTP-FLV | `src/http_flv/mod.rs` | Enhanced RTMP HEVC 标签，或 H265 输入时转码/拒绝并文档说明 |
| WebRTC | `src/webrtc/` | 发布：HEVC RTP depacketize（若对端支持）；播放：检测 H265 帧 → 触发转码子流或返回不支持 |

### 3.4 建议目录

```
src/codec/
  mod.rs
  h264.rs      # 从 webrtc/h264_util.rs 抽取共用
  hevc.rs      # VPS/SPS/PPS、NAL 类型、关键帧
  aac.rs
  parameter_set.rs  # 统一参数集管理
```

### 3.5 实施阶段

| 阶段 | 内容 | 优先级 |
|------|------|--------|
| P0 | 核心 VPS/SPS/PPS + RTSP H265 推拉 + HLS HEVC PMT | 高 |
| P1 | RTMP HVCC 推拉 | 高 |
| P2 | HTTP-FLV Enhanced HEVC 或明确仅 H264 | 中 |
| P3 | WebRTC H265（依赖浏览器与转码兜底） | 低 |

---

## 4. 视频录制

### 4.1 目标

- 对指定 `stream_id` 按时间或事件录制为可回放文件（fMP4 / MPEG-TS / MP4）。
- 与直播 HLS 滑动窗口分离：支持更长保留、按时间段查询与下载。
- 支持手动启停、计划任务、关键帧对齐切片。

### 4.2 架构设计

```
StreamManager.subscribe(stream_id)
        │
        ▼
┌───────────────────┐
│  RecorderSession  │  ← HTTP API 创建/停止
│  · 格式选择        │
│  · 切片策略        │
│  · 存储路径        │
└─────────┬─────────┘
          ▼
┌───────────────────┐
│  SegmentWriter    │  fMP4 / TS / MP4 muxer
│  · 索引 (sqlite)   │
└───────────────────┘
```

**注意命名：** RTSP 的 `RECORD` 方法表示**推流接入**，与文件录制无关。Rust 模块使用 `DvrRecorder` / `FileRecorder`，避免与 RTSP RECORD 混淆。

### 4.3 模块规划

```
src/record/
  mod.rs           # RecorderManager
  session.rs       # 单路录制会话，订阅 broadcast
  muxer/
    fmp4.rs        # 推荐：便于 HTTP Range 回放
    mpegts.rs      # 与 HLS 复用 ts_muxer 逻辑
    mp4.rs         # 整文件归档
  index.rs         # 录制元数据：起止时间、路径、stream_id
  storage.rs       # 目录布局、过期清理
```

### 4.4 存储布局

```
./recordings/
  <stream_id>/
    <date>/
      <start_ts>_<end_ts>.mp4
      index.json          # 或 SQLite 全局索引
```

### 4.5 配置扩展

```toml
[record]
enabled = true
base_dir = "./recordings"
default_format = "fmp4"      # fmp4 | ts | mp4
segment_duration_sec = 300     # 切片时长
max_retention_days = 30
align_keyframe = true
```

### 4.6 HTTP API（规划）

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/api/record/start` | `{stream_id, format?, segment_duration?}` |
| POST | `/api/record/stop` | `{stream_id}` 或 `{session_id}` |
| GET | `/api/recordings` | 列表查询（stream_id、时间范围） |
| GET | `/api/recordings/<id>/playback` | 回放 URL 或文件下载 |

### 4.7 与 HLS 的关系

| | 直播 HLS | 录制 |
|--|----------|------|
| 目的 | 低延迟播放 | 持久化存档 |
| 保留 | `max_segments` 滑动窗口 | 按策略长期保留 |
| 触发 | 首个 m3u8 请求 | API / 配置自动 |
| 复用 | `ts_muxer.rs` 可共享封装逻辑 | 独立 `record/` 模块与索引 |

---

## 5. 视频转码

### 5.1 目标

- 输入任意已发布流，输出指定编码/分辨率/码率的**衍生流**。
- 支持多路输出（如 `live_hd`、`live_sd`、WebRTC 用 H264）。
- 配置驱动，与 `config.toml` 中 `streams.outputs` 设计对齐。

### 5.2 架构设计

转码是**唯一需要解码**的子系统，不能放在 `publish_frame` 热路径上。

```
主流 subscribe
      │
      ▼
┌─────────────────┐     ┌──────────────────┐
│ TranscodeWorker │────►│ 衍生流 publish    │
│ (独立 task)      │     │ stream_id=live_sd │
└─────────────────┘     └──────────────────┘
      │
      ├── 方案 A: FFmpeg 子进程 (推荐首期)
      ├── 方案 B: GStreamer pipeline
      └── 方案 C: 硬件编码 (VideoToolbox / NVENC / QSV)
```

### 5.3 模块规划

```
src/transcode/
  mod.rs              # TranscodeManager
  worker.rs           # 单路转码任务生命周期
  ffmpeg.rs           # FFmpeg 进程封装（stdin/stdout 或 pipe）
  profile.rs          # 转码参数：codec、bitrate、resolution、fps
  filter.rs           # 水印、帧率控制（对接 config filters）
```

### 5.4 数据流

1. `TranscodeWorker` 订阅源 `stream_id`。
2. 将 Annex B / AAC 帧写入 FFmpeg（`-f h264` pipe 或临时 FIFO）。
3. FFmpeg 输出目标编码，worker 读回并 `publish_frame` 到目标 `stream_id`。
4. 分发层（FLV/HLS/RTSP 等）对衍生流与普通流无差别处理。

### 5.5 配置落地

将 `config.toml` 现有结构纳入 `StreamConfig`：

```toml
[[streams]]
id = "live"
source = "Push"

[[streams.outputs]]
protocol = "RTMP"
endpoint = "rtmp://127.0.0.1:1935/live_sd"
enabled = true
derivative_stream_id = "live_sd"    # 新增：本地衍生流 ID

[streams.outputs.transcode]
codec = "H264"
bitrate = 1000000
resolution = [640, 360]
fps = 25

[[streams.outputs.filters]]
type = "watermark"
enabled = true
text = "Media Server"
```

`src/core/config.rs` 需新增 `TranscodeConfig`、`OutputConfig`、`FilterConfig` 等结构体，并在 `main.rs` 启动时注册转码 worker。

### 5.6 性能与隔离

- 每路转码独立 tokio task + 进程，崩溃不影响主流。
- 资源上限：`[transcode] max_workers = 4`，超出排队或拒绝。
- H265→H264 转码为 WebRTC 播放的**标准兜底路径**。

---

## 6. 视频分析

### 6.1 目标

- 对码流或解码帧进行实时/准实时分析：场景检测、运动检测、质量评估、元数据提取。
- 不阻塞直播热路径；分析结果通过 HTTP API / Webhook / 消息队列输出。

### 6.2 分析层级

| 层级 | 说明 | 依赖 | 延迟 |
|------|------|------|------|
| L1 码流级 | SPS/PPS 解析、分辨率/帧率、码率估算、GOP 统计 | 无解码 | 极低 |
| L2 帧级 | 关键帧采样、画面变化检测（哈希/直方图） | 可选轻量解码 | 低 |
| L3 内容级 | 目标检测、人脸识别、OCR 等 | 解码 + 推理引擎 | 较高 |

首期建议 **L1 + L2 抽样**，L3 通过插件化接入。

### 6.3 架构设计

```
StreamManager.subscribe(stream_id)
        │
        ▼
┌────────────────────┐
│  AnalysisPipeline  │
│  · sampler (每 N 帧 / 仅 IDR) │
│  · 插件链            │
└─────────┬──────────┘
          ▼
┌────────────────────┐
│  AnalysisResult    │ → HTTP / Webhook / 内部 channel
└────────────────────┘
```

### 6.4 模块规划

```
src/analysis/
  mod.rs
  pipeline.rs       # 订阅 + 抽样 + 调度
  metrics.rs        # L1：码率、帧率、丢帧、GOP
  motion.rs         # L2：帧差 / 场景切换
  plugin.rs         # L3 插件 trait
  plugins/
    noop.rs
    # 未来：onnx、tensorrt 等
  sink.rs           # 结果输出：日志、HTTP POST、Redis
```

### 6.5 插件接口（草案）

```rust
pub trait AnalysisPlugin: Send + Sync {
    fn name(&self) -> &str;
    /// 码流级：每帧可选调用，应快速返回
    fn on_frame(&self, frame: &MediaFrame, ctx: &AnalysisContext) -> Option<AnalysisEvent>;
    /// 帧级：需要解码后的 RGB/YUV 时由 pipeline 提供
    fn on_decoded_frame(&self, image: &[u8], meta: &FrameMeta) -> Option<AnalysisEvent>;
}
```

### 6.6 HTTP API（规划）

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/api/analysis/start` | `{stream_id, plugins[], sample_interval?}` |
| POST | `/api/analysis/stop` | `{stream_id}` |
| GET | `/api/analysis/<stream_id>/metrics` | 实时指标 |
| GET | `/api/analysis/<stream_id>/events` | 事件列表（分页） |

### 6.7 与转码的关系

- L1 分析直接订阅主流，零转码。
- L3 可与转码 worker 共用 FFmpeg 解码出口，避免重复解码。

---

## 7. GB28181 接入

### 7.1 目标

- 作为 **SIP 下级平台/设备接入模块**，接收上级平台或 IPC 注册、目录查询、实时点播、回放（后期）。
- 媒体流为 **PS over RTP**（国标常见封装），解复用后注入 `StreamManager`。
- 支持向国标平台上报目录、心跳、告警（后期）。

### 7.2 国标交互概览

```
上级平台 / 客户端                vcp-media-server (本平台)
      │                                │
      │──── SIP REGISTER ─────────────►│  设备/平台注册
      │◄─── 200 OK ────────────────────│
      │──── MESSAGE (Catalog) ────────►│  目录查询
      │◄─── 200 OK + Catalog XML ──────│
      │──── INVITE (实时视频) ──────────►│  点播指定 channel
      │◄─── 200 OK + SDP ──────────────│
      │◄══ RTP/PS 或 ══ RTP/PS ═══════►│  媒体收发
      │──── BYE ───────────────────────►│  结束会话
```

### 7.3 架构设计

```
src/gb28181/
  mod.rs
  config.rs         # SIP ID、realm、本地 IP、媒体端口范围
  sip/
    server.rs       # UDP/TCP SIP 监听（可用 rsip + 自研事务层）
    register.rs     # REGISTER 处理
    catalog.rs      # Catalog 查询响应
    invite.rs       # INVITE / ACK / BYE
    message.rs      # MANSCDP XML 解析
  media/
    ps_demux.rs     # PS 解复用 → H264/H265 + G711/AAC NAL
    ps_mux.rs       # 国标级联输出（后期）
    rtp.rs          # RTP 收发、SSRC、端口管理
  device.rs         # 设备/通道模型，映射 stream_id
  session.rs        # 单路国标会话 ↔ StreamManager
```

**stream_id 映射规则：**

```
stream_id = "{device_id}_{channel_id}"
# 例：34020000001320000001_34020000001320000001
```

### 7.4 媒体路径

**接入（IPC → 本平台）：**

```
SIP INVITE (平台发起点播 IPC)
    → 本平台作为 SIP UAC 向 IPC 发 INVITE
    → 收到 RTP/PS
    → ps_demux → MediaFrame (Annex B)
    → StreamManager.publish_frame
    → 现有 RTSP/FLV/HLS/WebRTC 分发
```

**输出（本平台 → 上级平台）：**

```
上级 INVITE 本平台 channel
    → 从 StreamManager.subscribe 取帧
    → ps_mux + RTP
    → 回传上级平台
```

### 7.5 配置扩展

```toml
[gb28181]
enabled = true
sip_id = "34020000002000000001"      # 20 位平台编码
realm = "3402000000"
local_ip = "192.168.1.100"
sip_port = 5060
media_port_min = 30000
media_port_max = 30500
register_interval_sec = 3600
catalog_interval_sec = 60
```

### 7.6 HTTP API（规划）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/gb28181/devices` | 已注册设备/通道列表 |
| POST | `/api/gb28181/play` | `{device_id, channel_id}` 触发点播拉流 |
| POST | `/api/gb28181/stop` | 结束指定通道 |
| GET | `/api/gb28181/catalog` | 主动刷新目录 |

### 7.7 依赖选型

| 组件 | 建议 | 说明 |
|------|------|------|
| SIP 协议 | `rsip` + 自研事务/对话管理 | 纯 Rust，可控 |
| PS 解复用 | `mpeg2ts` / FFmpeg libav | PS 与 TS 同属 MPEG-2 Systems |
| XML | `quick-xml` | Catalog / MANSCDP |
| 信令传输 | UDP + TCP 双栈 | 国标常见双模式 |

### 7.8 与现有 RTSP 的关系

国标实时流本质是 **SIP 信令 + RTP 媒体**，与 RTSP 并行，独立模块。解复用后的帧统一进入 `StreamManager`，分发层复用现有 RTSP/FLV/HLS 能力，无需改动 Hub 核心。

---

## 8. 目标目录结构

```
src/
  main.rs
  core/
    mod.rs              # StreamManager、MediaFrame
    config.rs           # 扩展：record、transcode、gb28181、analysis
    protocol.rs         # 落地 StreamSink / StreamSource
    codec/              # h264、hevc、aac 共用工具
  rtmp/
  rtsp/
  webrtc/
  http/
  hls/
  http_flv/
  record/               # 新增：录制
  transcode/            # 新增：转码
  analysis/             # 新增：分析
  gb28181/              # 新增：国标接入
      sip/
      media/
```

---

## 9. 配置统一规划

`config.toml` 与 `Config` 结构体一一对应，分阶段启用：

| 配置段 | 状态 | 阶段 |
|--------|------|------|
| `[rtmp]` `[rtsp]` `[http]` `[hls]` | 已生效 | — |
| `[hub]` | 占位 | Phase 1 |
| `[record]` | 新增 | Phase 2 |
| `[[streams.outputs]]` + transcode | 占位 | Phase 3 |
| `[[streams.filters]]` | 占位 | Phase 3 |
| `[gb28181]` | 新增 | Phase 4 |
| `[analysis]` | 新增 | Phase 5 |

---

## 10. 实施路线图

```
Phase 1 — 基础加固 (1～2 周)
├── 抽取 src/codec/，统一 H264 工具
├── 落地 StreamSink / StreamSource trait
├── 解析 config [hub]、完善 Stream 元数据
└── H265：核心 VPS/SPS/PPS + RTSP 推拉

Phase 2 — 录制 (2～3 周)
├── src/record/ 模块
├── fMP4 / TS 切片写入与索引
├── HTTP API：start/stop/list/playback
└── 与直播 HLS 解耦验证

Phase 3 — 转码 (3～4 周)
├── src/transcode/ + FFmpeg worker
├── config streams.outputs 落地
├── 衍生流 publish（live_sd 等）
└── H265→H264 兜底（WebRTC）

Phase 4 — GB28181 (4～6 周)
├── SIP 注册与 Catalog
├── PS demux → StreamManager
├── 实时点播 INVITE 流程
└── 上级平台级联输出（PS mux）

Phase 5 — 视频分析 (2～3 周)
├── L1 码流指标
├── L2 关键帧抽样分析
├── 插件接口 + HTTP 事件 API
└── 可选：与转码共用解码链路
```

---

## 11. 风险与约束

| 风险 | 说明 | 缓解 |
|------|------|------|
| 转码 CPU 占用 | 多路 FFmpeg 耗资源 | worker 上限、硬件编码、仅按需转码 |
| WebRTC H265 | 浏览器支持有限 | 自动衍生 H264 子流 |
| GB28181 兼容性 | 厂商 PS 封装差异 | FFmpeg 解复用兜底 + 兼容测试矩阵 |
| 录制 IO | 高码率磁盘压力 | 切片、异步写、存储配额 |
| 热路径阻塞 | 分析/转码误入 publish 路径 | 严格 subscribe 旁路架构 |
| RTSP RECORD 命名 | 与文件录制混淆 | 模块命名 `DvrRecorder` vs `RtspPublish` |

---

## 12. 总结

| 能力 | 架构策略 | 关键模块 |
|------|----------|----------|
| H.265 | 扩展参数集与各协议打包/解包，Hub 仍用 Annex B + CodecType | `src/codec/hevc.rs`、RTMP/RTSP/HLS |
| 录制 | broadcast 旁路订阅，独立 muxer 与索引 | `src/record/` |
| 转码 | 独立 worker，衍生流回注 Hub | `src/transcode/` |
| 分析 | 抽样订阅 + 插件链，结果走 API | `src/analysis/` |
| GB28181 | 独立 SIP+PS 接入层，媒体统一入 Hub | `src/gb28181/` |

**核心原则：** 保持 `StreamManager` 作为唯一媒体枢纽，新能力均以 **订阅/发布** 模式挂接，控制面统一走 HTTP API 与 config，避免破坏现有 RTMP/RTSP/WebRTC/FLV/HLS 透传链路。
