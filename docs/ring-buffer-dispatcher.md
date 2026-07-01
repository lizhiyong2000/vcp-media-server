# RingBuffer + Dispatcher 媒体中枢演进方案

本文档描述 vcp-media-server **媒体 Hub 层**从现有 `broadcast + GOP 缓存` 演进到 **FrameRing（有界存储）+ FrameDispatcher（订阅调度）** 的设计分析，供实现前评审与迭代参考。

相关代码现状见 `src/core/mod.rs`、`src/core/broadcast_edge.rs`、`src/core/protocol.rs`；总体架构背景见 [architecture.md](./architecture.md)。

---

## 1. 现状与问题

### 1.1 当前架构

Hub-and-Spoke 模型：各协议 ingest 调用 `StreamManager::publish_frame`，egress 通过 `subscribe()` 获得 `tokio::sync::broadcast::Receiver<MediaFrame>`，各自独立消费。

```
RTMP play  ──subscribe()──┐
HTTP-FLV   ──subscribe()──┼── broadcast::Receiver<MediaFrame> ── 各自 loop
RTSP PLAY  ──subscribe()──┤
HLS        ──subscribe()──┤
WebRTC     ──subscribe()──┘
```

### 1.2 核心组件

| 组件 | 位置 | 作用 |
|------|------|------|
| `broadcast::channel(2048)` | `StreamManager::set_stream_broadcast` | 实时 fan-out，每 slot 存完整 `MediaFrame` 克隆 |
| `gop_frames: Vec<MediaFrame>` | `Stream` | 当前 GOP 全量缓存，**无上限** |
| `last_keyframe` / `last_keyframe_ts` | `Stream` | 最近 IDR，供 late join |
| `broadcast_edge` | `src/core/broadcast_edge.rs` | `drain_broadcast_lag`、`recv_flv_batch` 等 live-edge 工具 |
| `get_recent_gop_for_play` | `StreamManager` | GOP 回放 API，**已实现但未接入任何模块** |

`publish_frame` 热路径（简化）：

```rust
// src/core/mod.rs
if frame.is_keyframe { stream.last_keyframe = Some(frame.data.to_vec()); }
Self::update_gop_buffer(stream, &frame);  // gop_frames.push(frame.clone())
tx.send(frame);                           // broadcast
```

### 1.3 主要问题

| 问题 | 影响 |
|------|------|
| **双重/三重内存** | `gop_frames.clone()` + broadcast 克隆 + `last_keyframe.to_vec()` |
| **GOP 缓存无界** | WebRTC 浏览器长 GOP（2~10s）时 `gop_frames` 持续增长 |
| **broadcast Lagged** | 慢消费者丢历史，需各协议自行 `drain` + `request_publisher_keyframe` |
| **调度逻辑分散** | priming / coalesce / live-edge 在 RTMP、FLV、RTSP、HLS、WebRTC 各自实现 |
| **trait 未落地** | `StreamSink` / `StreamReceiver` 已定义，未接入实际分发路径 |

---

## 2. 目标

1. **有界内存**：按 GOP 或字节上限缓存，不再无限增长 `gop_frames`
2. **单一事实源**：late join、IDR priming 统一从 Ring 读取
3. **少拷贝**：payload 使用 `Bytes` 共享，避免重复 `to_vec()` / `clone()`
4. **调度集中**：用 `DispatchPolicy` 收敛各协议差异，删除重复的 `broadcast_edge` 逻辑
5. **可扩展**：为录制、回看、分析预留按序号/时间读历史帧能力

---

## 3. 目标架构：存储与调度分离

```
┌─────────────────────────────────────────────────────────┐
│  Ingest（RTMP / RTSP / WebRTC publish）                  │
│       publish_frame(frame)                               │
└───────────────────────────┬─────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────┐
│  StreamHub（per stream_id）                              │
│  ┌─────────────┐    ┌──────────────────┐                │
│  │  FrameRing  │───▶│ FrameDispatcher  │                │
│  │  有界存储    │    │ 订阅调度 + Policy │                │
│  └─────────────┘    └────────┬─────────┘                │
│                              │ seq 通知 (watch/broadcast) │
└──────────────────────────────┼──────────────────────────┘
                               │
         ┌─────────────────────┼─────────────────────┐
         ▼                     ▼                     ▼
    RTMP Sink            HTTP-FLV Sink           HLS Sink
    (StreamSink)         (StreamSink)            (StreamSink)
         │                     │                     │
    RTSP PLAY            WebRTC play            录制/分析（预留）
```

| 组件 | 职责 | 不负责 |
|------|------|--------|
| **FrameRing** | 有界帧存储、IDR 索引、GOP 边界 evict、共享 `Bytes` | 协议编码、会话管理 |
| **FrameDispatcher** | 注册会话、维护 cursor、按 Policy 选帧、唤醒 egress | RTMP/FLV/TS 打包 |
| **StreamSink** | `on_frame` 协议转换（已有 trait） | 订阅与读 Ring |

**设计原则：** 不建议用 RingBuffer **完全替换** async 唤醒机制；采用 **Ring 存历史 + 轻量 seq 通知** 的混合方案。

---

## 4. FrameRing 设计

### 4.1 存储单元

```rust
/// 轻量元数据 + 共享 payload
struct StoredFrame {
    seq: u64,              // 单调递增，全局序号
    track_id: u8,
    timestamp: u64,
    codec: CodecType,
    flags: FrameFlags,     // KEYFRAME | CONFIG | AUDIO
    data: Bytes,
}

struct FrameRing {
    capacity_frames: usize,      // 如 512
    capacity_bytes: usize,       // 如 32 << 20 (32MB)
    slots: VecDeque<StoredFrame>,
    idr_seqs: VecDeque<u64>,     // 最近 IDR 的 seq
    gop_start_seq: Option<u64>,  // 当前 GOP 起始
    write_seq: u64,
    bytes_used: usize,
}
```

与现有 `MediaFrame` 的关系：hub 内写入 Ring 一次；egress 按需 `StoredFrame → MediaFrame`（或只读 `FrameView`）。

### 4.2 Push 与 Evict

```
push(frame):
  if keyframe: 记录 gop_start_seq，push idr_seqs
  while over_capacity && can_evict_whole_gop:
      pop_front_gop()   // 必须从完整 GOP 边界删除，不可 mid-GOP
  slots.push_back(stored)
  write_seq += 1
  return seq
```

**H264 约束：** 不能在 GOP 中间 evict，否则引用链断裂。

| 策略 | 说明 |
|------|------|
| 按完整 GOP 保留 | Ring 至少保留最近 1~2 个完整 GOP |
| 按帧数 | 如 512 帧（~17s @ 30fps），适合 RTMP 短 GOP |
| 按字节 | 如 32MB，适合 1080p 大 I 帧 |
| WebRTC 长 GOP | 推荐 `max_gops = 2`，比纯帧数 cap 更稳 |

### 4.3 读 API

```rust
fn get(seq: u64) -> Option<&StoredFrame>
fn latest_seq() -> u64
fn latest_idr() -> Option<u64>
fn snap(mode: SnapMode) -> u64

enum SnapMode {
    LiveEdge,           // write_seq（最新）
    LatestIdr,          // 最近 IDR
    IdOrLive(u64),      // max(requested, latest_idr)，防 P 帧起播
}
```

### 4.4 与纯 broadcast 的对比

| 维度 | 现状 broadcast + Vec | FrameRing |
|------|---------------------|-----------|
| 内存 | 无界 GOP + 2048×订阅者队列 | 有界、单份 payload |
| Late join | 丢历史 + 多处 priming | `snap(LatestIdr)` 一次搞定 |
| 慢消费者 | 频繁 Lagged | cursor 可跳 live edge，Ring 内历史仍可读 |
| 实现复杂度 | 低 | 中（GOP 边界 + 通知） |

---

## 5. FrameDispatcher 设计

### 5.1 会话模型

每个 egress 连接对应一个 **DispatchSession**：

```rust
struct DispatchSession {
    id: SessionId,
    stream_id: String,
    protocol: ProtocolType,
    policy: DispatchPolicy,
    cursor: u64,              // 下一个待读 seq
    state: SessionState,      // Priming | Streaming | Lagged
    wake: watch::Receiver<u64>,
}

enum DispatchPolicy {
    /// RTMP / HTTP-FLV：追到 live edge，视频 burst 合并
    LiveCoalesce { video: bool, audio: bool },

    /// HLS：从 IDR 起顺序读每一帧，不跳帧
    SequentialFromIdr,

    /// WebRTC play：IDR 起播 + 视频 coalesce + 音频顺序
    WebRtcPlay,

    /// 录制 / 分析预留：从指定 seq 顺序读
    SequentialFromSeq(u64),
}
```

### 5.2 通知机制

替换 `broadcast::Sender<MediaFrame>` 为轻量 seq 通知：

```rust
struct StreamHub {
    ring: Arc<RwLock<FrameRing>>,
    seq_notify: watch::Sender<u64>,   // 或 broadcast::Sender<u64> 容量 64
}
```

**publish 路径：**

```rust
let seq = ring.write().push(frame)?;
seq_notify.send(seq)?;   // O(1)，不克隆 payload
```

**订阅者循环：**

```rust
let mut cursor = ring.snap(LatestIdr);
loop {
    while cursor <= ring.latest_seq() {
        if let Some(f) = ring.get(cursor) { sink.on_frame(f).await?; }
        cursor += 1;
    }
    wake.changed().await?;
}
```

对比 `broadcast(2048)`：通知 channel 只传 `u64`，消除 2048 帧 × payload 的克隆压力。

### 5.3 实现形态

| 形态 | 说明 | 适用 |
|------|------|------|
| **Centralized** | 每 stream 一个 dispatcher task，统一 poll 所有 session | 会话数中等，逻辑集中 |
| **Decentralized** | 每 session 独立 task，共享 `Arc<FrameRing>` + `watch::Receiver` | 与现有 `tokio::spawn` 接近，**迁移成本低** |

**推荐 Phase 1 采用 Decentralized**：各协议仍 spawn 自己的 play task，但读 Ring + 统一 `DispatchPolicy`，而非各自 `broadcast::recv`。

### 5.4 统一 Priming

```rust
fn attach_session(hub: &StreamHub, policy: DispatchPolicy) -> DispatchSession {
    request_publisher_keyframe(&hub.stream_id);
    let cursor = hub.ring.read().snap(LatestIdr);
    DispatchSession { cursor, policy, state: Priming, .. }
}
```

替代现有分散逻辑：

- `rtsp/play_egress.rs::prime_rtsp_play_rx`
- `webrtc/player.rs::prime_play_idr`
- 各模块内的 `drain_broadcast_lag` + 手动 IDR 等待

---

## 6. 各协议 Policy 映射

| 协议 | Policy | Snap 起点 | 读帧方式 | 被替代的现有逻辑 |
|------|--------|-----------|----------|------------------|
| RTMP play | `LiveCoalesce` | `LatestIdr` 起播后切 live | 视频 coalesce，音频顺序 | `prime_rtsp_play_rx` + `recv_flv_batch` |
| HTTP-FLV | `LiveCoalesce` | 同上 | 同上 | 同上 |
| RTSP PLAY | `SequentialFromIdr` | `LatestIdr` | 顺序，含 SPS/PPS 注入 | `play_egress::prime_rtsp_play_rx` |
| HLS | `SequentialFromIdr` | `LatestIdr` | 全帧顺序，不丢 | `drain_broadcast_lag` + priming |
| WebRTC play | `WebRtcPlay` | `LatestIdr` + keyframe 请求 | 视频 coalesce + RTP 时间线 | `prime_play_idr` + drain |

---

## 7. 与 StreamSink trait 衔接

`src/core/protocol.rs` 已定义：

```rust
#[async_trait]
pub trait StreamSink: Send + Sync {
    async fn on_frame(&mut self, frame: &MediaFrame) -> Result<Vec<u8>>;
    async fn generate_header(&self, stream: &Stream) -> Result<Vec<u8>>;
}
```

Dispatcher 只负责按 Policy 喂帧；各协议实现 `StreamSink`：

```rust
struct RtmpPlaySink { clock: RtmpPlayClock, .. }

#[async_trait]
impl StreamSink for RtmpPlaySink {
    async fn on_frame(&mut self, frame: &MediaFrame) -> Result<Vec<u8>> {
        // 现有 frame_to_rtmp_video / frame_to_rtmp_audio
    }
}

// 连接建立
let mut sink = RtmpPlaySink::new(...);
let session = hub.attach(LiveCoalesce, sink);
session.run().await;
```

HLS 可包装为 `HlsSegmentSink`：内部顺序读 Ring，`on_frame` 驱动 TS muxer 而非直接写网络。

---

## 8. StreamManager 演进结构

```rust
pub struct StreamManager {
    streams: RwLock<HashMap<StreamId, Stream>>,      // 元数据 only；移除 gop_frames
    hubs: RwLock<HashMap<StreamId, Arc<StreamHub>>>,
    receivers: RwLock<HashMap<ReceiverId, StreamReceiver>>,
}

pub struct StreamHub {
    stream_id: String,
    ring: RwLock<FrameRing>,
    seq_notify: watch::Sender<u64>,
}
```

`publish_frame` 演进：

```rust
pub fn publish_frame(&self, frame: MediaFrame) {
    update_stream_metadata(&frame);  // sps/pps/status
    if let Some(hub) = self.hubs.read().get(&stream_id) {
        hub.publish(frame);
    }
}
```

**可删除的冗余字段（Phase 2 后）：**

- `Stream.gop_frames`
- `Stream.last_keyframe` / `last_keyframe_ts`
- `StreamManager::get_gop_frames` / `get_recent_gop_for_play`（改由 Ring API 提供）

---

## 9. 并发与性能

### 9.1 锁策略

```
publish_frame（热路径）:
  1. ring.write().push()     // 短临界区
  2. seq_notify.send(seq)    // 无 payload

egress read:
  ring.read().get(seq)       // 共享读锁，多订阅者并行
```

Phase 2 优化：单写多读 + `AtomicU64 write_seq`，或 `Arc` snapshot。

### 9.2 内存估算（1080p @ 30fps，GOP = 2s）

| 方案 | 估算 |
|------|------|
| 现状 broadcast 2048 + gop_frames | 2048×克隆 + 无界 GOP，峰值可达数百 MB |
| Ring 2 GOP + 512 cap + 共享 Bytes | ~60 帧 × 30KB ≈ **2 MB / stream** |

---

## 10. 边界情况

| 场景 | 处理 |
|------|------|
| 无订阅者时 publish | Ring 仍写入（支持 late join）；可配置零订阅不写 Ring |
| 订阅者比 ingest 慢 | `LiveCoalesce` snap 到 live edge；`Sequential` 持续追帧 |
| Ring 内 IDR 已被 evict | `snap(LatestIdr)` 返回最旧可用 IDR + `request_publisher_keyframe` |
| 音视频双轨 | 单 Ring 按 seq 交错（与现状一致）；或 VideoRing + AudioRing 双 cursor |
| RTSP ingest 的 `rtp_data` | Ring 不存；RTSP 自用 side channel 或短期 broadcast |
| stream 重建 | `remove_stream` 销毁 hub；新 publish 创建新 Ring |

---

## 11. 迁移路线

### Phase 1（低风险）

- 新增 `src/core/frame_ring.rs`（push / get / snap / evict + 单元测试）
- 新增 `src/core/dispatch.rs`（`DispatchPolicy`、`attach_session`、读循环）
- `publish_frame` **双写** Ring + 现有 broadcast
- 提供 `snap_to_latest_idr()` 供 priming 试用

**风险：** 低，行为不变。

### Phase 2

- RTSP / FLV / RTMP priming 改读 Ring
- 删除 `gop_frames`、`last_keyframe` 及相关 API
- 回归 WebRTC 长 GOP 起播

**风险：** 中。

### Phase 3

- `broadcast<MediaFrame>` → seq-only notify
- HLS / WebRTC 全面接入 Dispatcher
- 删除 `src/core/broadcast_edge.rs`

**风险：** 中，需全协议回归。

### Phase 4

- 各协议 `StreamSink` 落地
- `StreamReceiver` 挂接 `DispatchSession`
- 录制 / 分析使用 `SequentialFromSeq`

**风险：** 低，结构清理。

### 建议首批改动

1. `frame_ring.rs` + GOP 边界单元测试
2. `dispatch.rs` + `DispatchPolicy`
3. `StreamHub` 嵌入 `StreamManager`，publish 双写
4. **先改 RTSP `play_egress`**（改动面小、priming 逻辑最清晰）

---

## 12. 实现选型

| 方案 | 适用性 |
|------|--------|
| **`VecDeque` + capacity + `Bytes`** | 最简单，与现有风格一致，**推荐 Phase 1** |
| `rtrb` / `crossbeam::ArrayQueue` | 无锁 MPMC，适合极高帧率，需自管 GOP 元数据 |
| 完全替换 broadcast | 需自建 condvar / 轮询，async 集成成本高，**不推荐先做** |

---

## 13. 结论

**RingBuffer + Dispatcher 是媒体 Hub 层的合理演进方向：**

1. **FrameRing** 解决有界内存、IDR 索引、GOP 完整性——针对 WebRTC 长 GOP 与 `gop_frames` 无界问题
2. **FrameDispatcher** 收敛各协议 lag / prime / coalesce 重复逻辑
3. **seq 通知** 替代 `broadcast<MediaFrame>`，热路径零 payload 拷贝
4. 与已有 `StreamSink` / `StreamReceiver` / `ProtocolType` 对齐，符合 [architecture.md](./architecture.md) Hub 演进方向

---

## 14. 参考文件

| 文件 | 说明 |
|------|------|
| `src/core/mod.rs` | `StreamManager`、`publish_frame`、`gop_frames`、`subscribe` |
| `src/core/broadcast_edge.rs` | live-edge 工具（迁移后拟删除） |
| `src/core/protocol.rs` | `StreamSink`、`ProtocolType` |
| `src/rtsp/play_egress.rs` | RTSP priming 参考实现 |
| `src/webrtc/player.rs` | WebRTC priming 参考实现 |
| `docs/architecture.md` | 总体架构与演进规划 |
