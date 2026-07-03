# 测试用例手册

本文档按 **视频编码** 与 **发布方式** 组织用例；每个发布场景均验证 **全部播放格式**。  
当前范围：**同编码透传**，不含转码。

## 文档结构

```
编码（H264 / H265）
 └── 发布方式（RTMP 推流、RTSP 推流、API 拉流、WebRTC 推流 …）
      └── 播放验证（RTMP / RTSP / HTTP-FLV / HLS / WebRTC）
```

| 维度 | 说明 |
|------|------|
| 编码 | H264+AAC、H265+AAC |
| 发布 | RTMP 推流、RTMP API 拉流、RTSP TCP/UDP 推流、RTSP API 拉流、WebRTC 推流 |
| 播放 | RTMP、RTSP、HTTP-FLV、HLS、WebRTC — 五种格式均须可播 |
| 转码 | **不在本手册范围** |

## 用例编号

| 编号 | 编码 | 发布方式 | stream_id（播放用） |
|------|------|----------|---------------------|
| TC-H264-PUB-01 | H264 | RTMP 推流 | `h264_rtmp` |
| TC-H264-PUB-02 | H264 | RTMP API 拉流 | `h264_pull_rtmp` |
| TC-H264-PUB-03 | H264 | RTSP TCP 推流 | `h264_rtsp_tcp` |
| TC-H264-PUB-04 | H264 | RTSP UDP 推流 | `h264_rtsp_udp` |
| TC-H264-PUB-05 | H264 | RTSP API 拉流（TCP） | `h264_pull_rtsp` |
| TC-H264-PUB-06 | H264 | RTSP API 拉流（UDP） | `h264_pull_udp` |
| TC-H264-PUB-07 | H264 | WebRTC 推流 | `h264_webrtc` |
| TC-H265-PUB-01 | H265 | RTMP 推流 | `h265_rtmp` |
| TC-H265-PUB-02 | H265 | RTMP API 拉流 | `h265_pull_rtmp` |
| TC-H265-PUB-03 | H265 | RTSP TCP 推流 | `h265_rtsp_tcp` |
| TC-H265-PUB-04 | H265 | RTSP UDP 推流 | `h265_rtsp_udp` |
| TC-H265-PUB-05 | H265 | RTSP API 拉流（TCP） | `h265_pull_rtsp` |
| TC-H265-PUB-06 | H265 | RTSP API 拉流（UDP） | `h265_pull_udp` |
| TC-H265-PUB-07 | H265 | WebRTC 推流 | `h265_webrtc` |

## 覆盖矩阵

**H264** — 7 种发布 × 5 种播放 = 35 项验证

| 发布 ↓ \ 播放 → | RTMP | RTSP | HTTP-FLV | HLS | WebRTC |
|-----------------|------|------|----------|-----|--------|
| RTMP 推流 | ✓ | ✓ | ✓ | ✓ | ✓ |
| RTMP API 拉流 | ✓ | ✓ | ✓ | ✓ | ✓ |
| RTSP TCP 推流 | ✓ | ✓ | ✓ | ✓ | ✓ |
| RTSP UDP 推流 | ✓ | ✓ | ✓ | ✓ | ✓ |
| RTSP API 拉流 TCP | ✓ | ✓ | ✓ | ✓ | ✓ |
| RTSP API 拉流 UDP | ✓ | ✓ | ✓ | ✓ | ✓ |
| WebRTC 推流 | ✓ | ✓ | ✓ | ✓ | ✓ |

**H265** — 7 种发布 × 5 种播放 = 35 项验证（WebRTC 推流/播放依赖浏览器 `video/H265` 或 `video/HEVC` 能力）

| 发布 ↓ \ 播放 → | RTMP | RTSP | HTTP-FLV | HLS | WebRTC |
|-----------------|------|------|----------|-----|--------|
| RTMP 推流 | ✓ | ✓ | ✓ | ✓ | ✓ |
| RTMP API 拉流 | ✓ | ✓ | ✓ | ✓ | ✓ |
| RTSP TCP 推流 | ✓ | ✓ | ✓ | ✓ | ✓ |
| RTSP UDP 推流 | ✓ | ✓ | ✓ | ✓ | ✓ |
| RTSP API 拉流 TCP | ✓ | ✓ | ✓ | ✓ | ✓ |
| RTSP API 拉流 UDP | ✓ | ✓ | ✓ | ✓ | ✓ |
| WebRTC 推流 | ✓ | ✓ | ✓ | ✓ | ✓ |

---

## 通用前置

**终端 1 — 启动服务（所有用例均需）：**

```bash
cd /path/to/vcp-media-server
cargo build
./target/debug/vcp-media-server
```

**默认端口：**

| 服务 | 端口 |
|------|------|
| RTMP | 1935 |
| RTSP | 554 |
| HTTP（API / FLV / HLS / 测试页） | 8081 |
| WebRTC 信令 | 9080 |

**WebRTC 测试页：** `http://127.0.0.1:8081/webrtc/webrtc-test.html`

**测试源统一参数：**

| 项 | 值 |
|----|-----|
| 分辨率 | 640×360 |
| 帧率 | 25 fps |
| GOP | `-g 25`（便于 HLS 切片） |
| 音频 | AAC 44100 Hz 立体声 |

**H265 / Enhanced RTMP 注意：**

- RTMP 推/播 H265 须加 `-rtmp_enhanced_codecs hvc1`
- **不要**使用 `-tag:v hvc1`（会与 FLV muxer 冲突）
- 日志关键字：`HEVC`、`HVCC SequenceHeader`、`sprop-vps/sps/pps`、HLS PMT `0x24`

---

## 命令速查

以下命令中的占位符：

| 占位符 | 含义 |
|--------|------|
| `{stream_id}` | 发布目标 / 播放用的流 ID |
| `{src_id}` | API 拉流时的远端源流 ID |

---

### H264 发布命令

#### RTMP 推流

```bash
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/{stream_id}
```

#### RTMP API 拉流

```bash
# 终端 2：模拟远端 RTMP 源
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/{src_id}

# 终端 3：触发拉流，转发到 {stream_id}
curl -X POST http://127.0.0.1:8081/api/rtmp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtmp://127.0.0.1:1935/live/{src_id}","stream_id":"{stream_id}"}'
```

#### RTSP TCP 推流

```bash
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 25 \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/{stream_id}
```

#### RTSP UDP 推流

```bash
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport udp rtsp://127.0.0.1:554/{stream_id}
```

#### RTSP API 拉流（TCP）

```bash
# 终端 2：RTSP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/{src_id}

# 终端 3：触发拉流
curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtsp://127.0.0.1:554/{src_id}","stream_id":"{stream_id}"}'
```

#### RTSP API 拉流（UDP）

```bash
# 终端 2：RTSP 源流（TCP 推源即可）
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/{src_id}

# 终端 3：UDP 拉流
curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtsp://127.0.0.1:554/{src_id}?transport=udp","stream_id":"{stream_id}"}'
```

#### WebRTC 推流

1. 浏览器打开 `http://127.0.0.1:8081/webrtc/webrtc-test.html`
2. 选择视频编码 H264 或 H265/HEVC
3. `stream_id` 填 `{stream_id}`，点击「连接信令」→「开始推流」，允许摄像头/麦克风

---

### H265 发布命令

#### RTMP 推流（Enhanced RTMP）

```bash
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx265 -preset ultrafast -tune zerolatency -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f flv -rtmp_enhanced_codecs hvc1 rtmp://127.0.0.1:1935/live/{stream_id}
```

#### RTMP API 拉流

```bash
# 终端 2：模拟远端 H265 RTMP 源
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx265 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f flv -rtmp_enhanced_codecs hvc1 rtmp://127.0.0.1:1935/live/{src_id}

# 终端 3：触发拉流
curl -X POST http://127.0.0.1:8081/api/rtmp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtmp://127.0.0.1:1935/live/{src_id}","stream_id":"{stream_id}"}'
```

#### RTSP TCP 推流

```bash
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx265 -preset ultrafast -tune zerolatency -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/{stream_id}
```

#### RTSP UDP 推流

```bash
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx265 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport udp rtsp://127.0.0.1:554/{stream_id}
```

#### RTSP API 拉流（TCP）

```bash
# 终端 2：H265 RTSP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx265 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/{src_id}

# 终端 3：触发拉流
curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtsp://127.0.0.1:554/{src_id}","stream_id":"{stream_id}"}'
```

#### RTSP API 拉流（UDP）

```bash
# 终端 2：H265 RTSP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx265 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/{src_id}

# 终端 3：UDP 拉流
curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtsp://127.0.0.1:554/{src_id}?transport=udp","stream_id":"{stream_id}"}'
```

---

### 播放命令

完成发布后，将 `{stream_id}` 替换为用例中的播放流 ID，依次验证五种格式。

#### RTMP 播放

```bash
# H264
ffplay -fflags nobuffer -flags low_delay -probesize 32 -analyzeduration 0 \
  rtmp://127.0.0.1:1935/live/{stream_id}

# H265（须 Enhanced RTMP）
ffplay -rtmp_enhanced_codecs hvc1 -fflags nobuffer -flags low_delay \
  rtmp://127.0.0.1:1935/live/{stream_id}
```

#### RTSP 播放

```bash
# TCP
ffplay -rtsp_transport tcp -fflags nobuffer -flags low_delay \
  rtsp://127.0.0.1:554/{stream_id}

# UDP（适用于 UDP 推流或 UDP 拉流场景）
ffplay -rtsp_transport udp rtsp://127.0.0.1:554/{stream_id}
```

#### HTTP-FLV 播放

```bash
ffplay -fflags nobuffer -flags low_delay \
  http://127.0.0.1:8081/flv/{stream_id}
```

#### HLS 播放

```bash
# 推流稳定后等待首片约 2–4 秒
ffplay http://127.0.0.1:8081/hls/{stream_id}/live.m3u8

# 可选：检查 m3u8 与切片目录
curl http://127.0.0.1:8081/hls/{stream_id}/live.m3u8
ls ./saving/hls/{stream_id}/
```

#### WebRTC 播放

1. 浏览器打开 `http://127.0.0.1:8081/webrtc/webrtc-test.html`
2. `stream_id` 填写 `{stream_id}`，点击「连接信令」→「开始播放」

> **H265 WebRTC 播放：** 客户端 SDP 须包含 `H265` 或 `HEVC` rtpmap；多数桌面 Chrome 不支持 H265 解码，可用支持 HEVC 的客户端或 ffplay 验证其余四种格式。

#### 无界面批量验证

```bash
STREAM={stream_id}

# H264
ffmpeg -i rtmp://127.0.0.1:1935/live/$STREAM -t 5 -f null -
ffmpeg -rtsp_transport tcp -i rtsp://127.0.0.1:554/$STREAM -t 5 -f null -
ffmpeg -i http://127.0.0.1:8081/flv/$STREAM -t 5 -f null -
ffmpeg -i http://127.0.0.1:8081/hls/$STREAM/live.m3u8 -t 5 -f null -

# H265（RTMP 须 Enhanced）
ffmpeg -rtmp_enhanced_codecs hvc1 -i rtmp://127.0.0.1:1935/live/$STREAM -t 5 -f null -
```

**播放通用预期：** 各播放端画面与声音正常；日志出现对应协议的首帧/首包关键字（如 `>>> SEND first frame`、`First access unit`、`Play streaming started`）。

---

# 第一章：H264 + AAC

各用例执行步骤：**发布** → 按 [播放命令](#播放命令) 验证全部五种格式。

| 编号 | 发布命令 | stream_id | src_id（仅 API 拉流） | 发布预期日志 |
|------|----------|-----------|----------------------|--------------|
| TC-H264-PUB-01 | [RTMP 推流](#rtmp-推流) | `h264_rtmp` | — | `[RTMP] Publishing`、`AVC SequenceHeader`、`>>> SEND first frame` |
| TC-H264-PUB-02 | [RTMP API 拉流](#rtmp-api-拉流) | `h264_pull_rtmp` | `h264_src_rtmp` | `[RTMP Puller] First relayed frame` |
| TC-H264-PUB-03 | [RTSP TCP 推流](#rtsp-tcp-推流) | `h264_rtsp_tcp` | — | `[RTSP] ANNOUNCE`、`[RTSP-Push] First access unit` |
| TC-H264-PUB-04 | [RTSP UDP 推流](#rtsp-udp-推流) | `h264_rtsp_udp` | — | `[RTSP-Push-UDP] First access unit` |
| TC-H264-PUB-05 | [RTSP API 拉流（TCP）](#rtsp-api-拉流tcp) | `h264_pull_rtsp` | `h264_src_rtsp` | `[RTSP Puller] SUCCESS`、`[RTSP-Pull] First access unit` |
| TC-H264-PUB-06 | [RTSP API 拉流（UDP）](#rtsp-api-拉流udp) | `h264_pull_udp` | `h264_src_rtsp` | `[RTSP Puller] Transport: UDP`、`[RTSP-Pull-UDP] First access unit` |
| TC-H264-PUB-07 | [WebRTC 推流](#webrtc-推流) | `h264_webrtc` | — | `Publish request`、`First published video frame` |

**示例 — TC-H264-PUB-01：**

1. 按 [H264 RTMP 推流](#rtmp-推流) 执行，`{stream_id}` = `h264_rtmp`
2. 按 [播放命令](#播放命令) 验证，`{stream_id}` = `h264_rtmp`

**示例 — TC-H264-PUB-02：**

1. 按 [H264 RTMP API 拉流](#rtmp-api-拉流) 执行，`{src_id}` = `h264_src_rtmp`，`{stream_id}` = `h264_pull_rtmp`
2. 按 [播放命令](#播放命令) 验证，`{stream_id}` = `h264_pull_rtmp`

---

# 第二章：H265 + AAC

各用例执行步骤：**发布** → 按 [播放命令](#播放命令) 验证全部五种格式（H265 RTMP 播放须 `-rtmp_enhanced_codecs hvc1`）。

| 编号 | 发布命令 | stream_id | src_id（仅 API 拉流） | 发布预期日志 |
|------|----------|-----------|----------------------|--------------|
| TC-H265-PUB-01 | [RTMP 推流（Enhanced RTMP）](#rtmp-推流enhanced-rtmp) | `h265_rtmp` | — | HEVC/HVCC sequence header、VPS/SPS/PPS 写入 |
| TC-H265-PUB-02 | [RTMP API 拉流](#rtmp-api-拉流-1) | `h265_pull_rtmp` | `h265_src_rtmp` | `[RTMP Puller] Stored HEVC VPS/SPS/PPS`、`First relayed frame` |
| TC-H265-PUB-03 | [RTSP TCP 推流](#rtsp-tcp-推流-1) | `h265_rtsp_tcp` | — | `[RTSP-Push] HEVC access unit`、HEVC SDP |
| TC-H265-PUB-04 | [RTSP UDP 推流](#rtsp-udp-推流-1) | `h265_rtsp_udp` | — | `[RTSP-Push-UDP] HEVC access unit` |
| TC-H265-PUB-05 | [RTSP API 拉流（TCP）](#rtsp-api-拉流tcp-1) | `h265_pull_rtsp` | `h265_src_rtsp` | `[RTSP Puller] SUCCESS`、HEVC VPS/SPS/PPS |
| TC-H265-PUB-06 | [RTSP API 拉流（UDP）](#rtsp-api-拉流udp-1) | `h265_pull_udp` | `h265_src_rtsp` | `[RTSP Puller] Transport: UDP`、HEVC 首帧 relay |
| TC-H265-PUB-07 | [WebRTC 推流](#webrtc-推流) | `h265_webrtc` | — | `Publish: prefer H265/HEVC`、`First HEVC RTP packet` |

**示例 — TC-H265-PUB-01：**

1. 按 [H265 RTMP 推流](#rtmp-推流enhanced-rtmp) 执行，`{stream_id}` = `h265_rtmp`
2. 按 [播放命令](#播放命令) 验证，`{stream_id}` = `h265_rtmp`（RTMP 播放加 `-rtmp_enhanced_codecs hvc1`）

**示例 — TC-H265-PUB-05：**

1. 按 [H265 RTSP API 拉流（TCP）](#rtsp-api-拉流tcp-1) 执行，`{src_id}` = `h265_src_rtsp`，`{stream_id}` = `h265_pull_rtsp`
2. 按 [播放命令](#播放命令) 验证，`{stream_id}` = `h265_pull_rtsp`

---

# 附录：常见问题

| 现象 | 处理 |
|------|------|
| RTMP StreamNotFound | 检查 app/stream_name；播放须晚于推流 |
| RTSP 404 | 流未 publish；DESCRIBE 前须先推流或拉流成功 |
| UDP 无画面 | 拉流 URL 加 `?transport=udp`；确认 RTP 分片（H264 FU-A / H265 FU） |
| HLS 无切片 | 先推流再请求 m3u8；检查 `config.toml` `[server.hls] enabled` |
| WebRTC 黑屏 | `stream_id` 一致；等待 IDR 关键帧 |
| H265 RTMP 推流失败 | ffmpeg ≥6 加 `-rtmp_enhanced_codecs hvc1`（勿用 `-tag:v hvc1`） |
| H265 RTMP 播放黑屏 | ffplay 也须 `-rtmp_enhanced_codecs hvc1` |
| H265 FLV/HLS 异常 | 检查 VPS/SPS/PPS 是否写入；HLS 视频流类型应为 `0x24` |
| H265 WebRTC 推流/播放失败 | 浏览器须支持 `video/H265` 或 `video/HEVC`；不支持时测试页会提示失败 |
| 端口占用 | `lsof -i :1935` / `:554` / `:8081` / `:9080` |
