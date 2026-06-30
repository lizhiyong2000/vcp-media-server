# 测试用例手册

本文档包含全部 **27** 项测试用例，编号 `TC-01`～`TC-27`。  
每项包含：说明、前置条件、操作步骤（完整命令）、预期结果。

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
| HTTP（API / FLV / HLS） | 8081 |
| WebRTC 信令 | 9080 |

**WebRTC 测试页：** `http://127.0.0.1:8081/webrtc/webrtc-test.html`

**测试源建议参数：** H264 + AAC，640×360@25fps，`-g 25`（便于 HLS 切片）。

---

## 用例总览

| 编号 | 类型 | 发布方式 | 播放方式 |
|------|------|----------|----------|
| TC-01 | 基础 | RTMP 推流 | RTMP 播放 |
| TC-02 | 基础 | RTMP API 拉流 | RTMP 播放 |
| TC-03 | 基础 | RTSP TCP 推流 | RTSP TCP 播放 |
| TC-04 | 基础 | RTSP UDP 推流 | RTSP UDP 播放 |
| TC-05 | 基础 | RTSP API 拉流（TCP） | RTSP TCP 播放 |
| TC-06 | 基础 | RTSP API 拉流（UDP） | HTTP-FLV 播放 |
| TC-07 | 基础 | WebRTC 推流 | WebRTC 播放 |
| TC-08 | 跨协议 | RTMP 推流 | RTSP 播放 |
| TC-09 | 跨协议 | RTMP 推流 | HTTP-FLV 播放 |
| TC-10 | 跨协议 | RTMP 推流 | HLS 播放 |
| TC-11 | 跨协议 | RTMP 推流 | WebRTC 播放 |
| TC-12 | 跨协议 | RTSP 推流 | RTMP 播放 |
| TC-13 | 跨协议 | RTSP 推流 | HTTP-FLV 播放 |
| TC-14 | 跨协议 | RTSP 推流 | HLS 播放 |
| TC-15 | 跨协议 | RTSP 推流 | WebRTC 播放 |
| TC-16 | 跨协议 | RTMP API 拉流 | RTSP 播放 |
| TC-17 | 跨协议 | RTMP API 拉流 | HTTP-FLV 播放 |
| TC-18 | 跨协议 | RTMP API 拉流 | HLS 播放 |
| TC-19 | 跨协议 | RTMP API 拉流 | WebRTC 播放 |
| TC-20 | 跨协议 | RTSP API 拉流 | RTMP 播放 |
| TC-21 | 跨协议 | RTSP API 拉流 | HTTP-FLV 播放 |
| TC-22 | 跨协议 | RTSP API 拉流 | HLS 播放 |
| TC-23 | 跨协议 | RTSP API 拉流 | WebRTC 播放 |
| TC-24 | 跨协议 | WebRTC 推流 | RTMP 播放 |
| TC-25 | 跨协议 | WebRTC 推流 | RTSP 播放 |
| TC-26 | 跨协议 | WebRTC 推流 | HTTP-FLV 播放 |
| TC-27 | 跨协议 | WebRTC 推流 | HLS 播放 |

---

# 第一部分：基础用例（同协议发布 + 播放）

## TC-01  RTMP 推流 + RTMP 播放

**说明：** 验证 RTMP 协议最基本的 publish / play 链路，ffmpeg 推测试源，ffplay 从同一 app/stream 拉流。

**stream_id：** `stream1`（RTMP URL 中为 `live/stream1`）

**步骤：**

```bash
# 终端 2：推流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/stream1

# 终端 3：播放（推流稳定后执行）
ffplay -fflags nobuffer -flags low_delay -probesize 32 -analyzeduration 0 \
  rtmp://127.0.0.1:1935/live/stream1
```

**预期：** 画面与声音正常；日志出现 `[RTMP] Publishing to stream`、`AVC SequenceHeader`、`>>> SEND first frame`。

---

## TC-02  RTMP API 拉流 + RTMP 播放

**说明：** 模拟从远端 RTMP 源拉流并转发到本地 `stream_id`，再用 RTMP 播放转发结果。验证 `/api/rtmp/pull` 与本地 relay。

**stream_id：** 源 `src_rtmp`，转发后 `pull_rtmp`

**步骤：**

```bash
# 终端 2：模拟远端 RTMP 源
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/src_rtmp

# 终端 3：触发 RTMP 拉流
curl -X POST http://127.0.0.1:8081/api/rtmp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtmp://127.0.0.1:1935/live/src_rtmp","stream_id":"pull_rtmp"}'

# 终端 4：播放转发流
ffplay rtmp://127.0.0.1:1935/live/pull_rtmp
```

**预期：** 日志 `[RTMP Puller] First relayed frame`；播放画面正常。

---

## TC-03  RTSP TCP 推流 + RTSP TCP 播放

**说明：** 验证 RTSP ANNOUNCE/RECORD 推流与 DESCRIBE/PLAY 播放（TCP interleaved）。

**stream_id：** `stream1`

**步骤：**

```bash
# 终端 2：推流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 25 \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/stream1

# 终端 3：播放
ffplay -rtsp_transport tcp -fflags nobuffer -flags low_delay \
  rtsp://127.0.0.1:554/stream1
```

**预期：** 日志 `[RTSP] ANNOUNCE`、`[RTSP-Push] First access unit`、`Handling PLAY`；播放正常。

---

## TC-04  RTSP UDP 推流 + RTSP UDP 播放

**说明：** 验证 RTSP UDP 传输模式下推流 ingest 与 PLAY 下发（RTP over UDP）。

**stream_id：** `stream_udp`

**步骤：**

```bash
# 终端 2：UDP 推流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport udp rtsp://127.0.0.1:554/stream_udp

# 终端 3：UDP 播放
ffplay -rtsp_transport udp rtsp://127.0.0.1:554/stream_udp
```

**预期：** 日志 `[RTSP-Push-UDP] First access unit`；UDP SETUP 端口成对分配；播放有画面。

---

## TC-05  RTSP API 拉流（TCP）+ RTSP TCP 播放

**说明：** 从本机 RTSP 源流经 HTTP API 拉取到 `pull_rtsp`，再以 RTSP TCP 播放。

**stream_id：** 源 `src_rtsp`，拉流后 `pull_rtsp`

**步骤：**

```bash
# 终端 2：RTSP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/src_rtsp

# 终端 3：API 拉流
curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtsp://127.0.0.1:554/src_rtsp","stream_id":"pull_rtsp"}'

# 终端 4：RTSP 播放
ffplay -rtsp_transport tcp rtsp://127.0.0.1:554/pull_rtsp
```

**预期：** 日志 `[RTSP Puller] SUCCESS`、`[RTSP-Pull] First access unit`；播放正常。

---

## TC-06  RTSP API 拉流（UDP）+ HTTP-FLV 播放

**说明：** 使用 UDP 模式从 RTSP 源拉流（URL 加 `transport=udp`），经 StreamManager 转发后 HTTP-FLV 播放。

**stream_id：** 源 `src_rtsp`，拉流后 `pull_udp`

**步骤：**

```bash
# 终端 2：RTSP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/src_rtsp

# 终端 3：UDP 拉流
curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtsp://127.0.0.1:554/src_rtsp?transport=udp","stream_id":"pull_udp"}'

# 终端 4：FLV 播放
ffplay http://127.0.0.1:8081/flv/pull_udp
```

**预期：** 日志 `[RTSP Puller] Transport: UDP`、`[RTSP-Pull-UDP] First access unit`；FLV 有音视频。

---

## TC-07  WebRTC 推流 + WebRTC 播放

**说明：** 浏览器经 WebSocket 信令完成 WebRTC publish 与 play，验证同协议端到端。

**stream_id：** `webrtc_base`

**步骤：**

```bash
# 终端 1 已启动服务
```

1. 浏览器打开 `http://127.0.0.1:8081/webrtc/webrtc-test.html`
2. `stream_id` 填写 `webrtc_base`，点击「连接信令」→「开始推流」，允许摄像头/麦克风
3. 新标签页打开同一测试页，`stream_id` 仍填 `webrtc_base`，连接信令 →「开始播放」

**预期：** 推流端与播放端均有画面；日志 `Publish request`、`First published video frame`、`First played video frame`。

---

# 第二部分：跨协议用例（不同协议发布 + 播放）

以下用例验证 StreamManager 统一广播：一种协议写入，另一种协议读出。

---

## TC-08  RTMP 推流 → RTSP 播放

**说明：** RTMP 发布，RTSP DESCRIBE/PLAY 播放。

**stream_id：** `pub_rtmp`

**步骤：**

```bash
# 终端 2：RTMP 推流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/pub_rtmp

# 终端 3：RTSP 播放
ffplay -rtsp_transport tcp rtsp://127.0.0.1:554/pub_rtmp
```

**预期：** RTSP 播放有画面；日志 `Starting RTP sender`。

---

## TC-09  RTMP 推流 → HTTP-FLV 播放

**说明：** RTMP 发布，HTTP-FLV 长连接播放。

**stream_id：** `pub_rtmp`

**步骤：**

```bash
# 终端 2：RTMP 推流（同上 TC-08）
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/pub_rtmp

# 终端 3：FLV 播放
ffplay http://127.0.0.1:8081/flv/pub_rtmp
```

**预期：** FLV 流可解码，画面正常。

---

## TC-10  RTMP 推流 → HLS 播放

**说明：** RTMP 发布，按需 HLS 切片后 m3u8 播放。

**stream_id：** `pub_rtmp`

**步骤：**

```bash
# 终端 2：RTMP 推流（同上，保持 -g 25）
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/pub_rtmp

# 终端 3：等待首片约 2–4 秒后播放
ffplay http://127.0.0.1:8081/hls/pub_rtmp/live.m3u8

# 可选：检查切片
curl http://127.0.0.1:8081/hls/pub_rtmp/live.m3u8
ls ./hls/pub_rtmp/
```

**预期：** m3u8 含 `#EXTINF` 与 `segment_*.ts`；播放正常。

---

## TC-11  RTMP 推流 → WebRTC 播放

**说明：** RTMP 发布，浏览器 WebRTC 订阅同一 stream。

**stream_id：** `pub_rtmp`

**步骤：**

```bash
# 终端 2：RTMP 推流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 -pix_fmt yuv420p \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 50 \
  -f flv rtmp://127.0.0.1:1935/live/pub_rtmp
```

1. 浏览器打开 `http://127.0.0.1:8081/webrtc/webrtc-test.html`
2. `stream_id` 填 `pub_rtmp`，连接信令 →「开始播放」

**预期：** 浏览器出现测试源画面；日志 `Play streaming started`。

---

## TC-12  RTSP 推流 → RTMP 播放

**说明：** RTSP ANNOUNCE/RECORD 发布，RTMP play 播放。

**stream_id：** `pub_rtsp`

**步骤：**

```bash
# 终端 2：RTSP 推流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/pub_rtsp

# 终端 3：RTMP 播放
ffplay rtmp://127.0.0.1:1935/live/pub_rtsp
```

**预期：** RTMP 播放正常；日志 `[RTSP-Push] First access unit`。

---

## TC-13  RTSP 推流 → HTTP-FLV 播放

**说明：** RTSP 发布，HTTP-FLV 播放。

**stream_id：** `pub_rtsp`

**步骤：**

```bash
# 终端 2：RTSP 推流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/pub_rtsp

# 终端 3：FLV 播放
ffplay http://127.0.0.1:8081/flv/pub_rtsp
```

**预期：** FLV 播放正常。

---

## TC-14  RTSP 推流 → HLS 播放

**说明：** RTSP 发布，HLS 切片播放。

**stream_id：** `pub_rtsp`

**步骤：**

```bash
# 终端 2：RTSP 推流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/pub_rtsp

# 终端 3：HLS 播放
ffplay http://127.0.0.1:8081/hls/pub_rtsp/live.m3u8
```

**预期：** m3u8 与 ts 文件生成；播放正常。

---

## TC-15  RTSP 推流 → WebRTC 播放

**说明：** RTSP 发布，浏览器 WebRTC 播放。

**stream_id：** `pub_rtsp`

**步骤：**

```bash
# 终端 2：RTSP 推流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 -pix_fmt yuv420p \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 50 -an \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/pub_rtsp
```

1. 浏览器打开 `http://127.0.0.1:8081/webrtc/webrtc-test.html`
2. `stream_id` 填 `pub_rtsp`，连接信令 → 开始播放

**预期：** 浏览器有画面。

---

## TC-16  RTMP API 拉流 → RTSP 播放

**说明：** RTMP 源经 API 拉取后，RTSP 协议播放。

**stream_id：** 源 `src_rtmp`，拉流后 `pub_rtmp_pull`

**步骤：**

```bash
# 终端 2：RTMP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/src_rtmp

# 终端 3：API 拉流
curl -X POST http://127.0.0.1:8081/api/rtmp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtmp://127.0.0.1:1935/live/src_rtmp","stream_id":"pub_rtmp_pull"}'

# 终端 4：RTSP 播放
ffplay -rtsp_transport tcp rtsp://127.0.0.1:554/pub_rtmp_pull
```

**预期：** 拉流转发成功；RTSP 播放正常。

---

## TC-17  RTMP API 拉流 → HTTP-FLV 播放

**说明：** RTMP 拉流转发后 HTTP-FLV 播放。

**stream_id：** 源 `src_rtmp`，拉流后 `pub_rtmp_pull`

**步骤：**

```bash
# 终端 2：RTMP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/src_rtmp

# 终端 3：API 拉流
curl -X POST http://127.0.0.1:8081/api/rtmp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtmp://127.0.0.1:1935/live/src_rtmp","stream_id":"pub_rtmp_pull"}'

# 终端 4：FLV 播放
ffplay http://127.0.0.1:8081/flv/pub_rtmp_pull
```

**预期：** FLV 播放正常。

---

## TC-18  RTMP API 拉流 → HLS 播放

**说明：** RTMP 拉流转发后 HLS 播放。

**stream_id：** 源 `src_rtmp`，拉流后 `pub_rtmp_pull`

**步骤：**

```bash
# 终端 2：RTMP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/src_rtmp

# 终端 3：API 拉流
curl -X POST http://127.0.0.1:8081/api/rtmp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtmp://127.0.0.1:1935/live/src_rtmp","stream_id":"pub_rtmp_pull"}'

# 终端 4：HLS 播放
ffplay http://127.0.0.1:8081/hls/pub_rtmp_pull/live.m3u8
```

**预期：** HLS 切片与播放正常。

---

## TC-19  RTMP API 拉流 → WebRTC 播放

**说明：** RTMP 拉流转发后浏览器 WebRTC 播放。

**stream_id：** 源 `src_rtmp`，拉流后 `pub_rtmp_pull`

**步骤：**

```bash
# 终端 2：RTMP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/src_rtmp

# 终端 3：API 拉流
curl -X POST http://127.0.0.1:8081/api/rtmp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtmp://127.0.0.1:1935/live/src_rtmp","stream_id":"pub_rtmp_pull"}'
```

1. 浏览器打开 `http://127.0.0.1:8081/webrtc/webrtc-test.html`
2. `stream_id` 填 `pub_rtmp_pull`，连接信令 → 开始播放

**预期：** 浏览器有画面。

---

## TC-20  RTSP API 拉流 → RTMP 播放

**说明：** RTSP 源经 API 拉取后 RTMP 播放。

**stream_id：** 源 `src_rtsp`，拉流后 `pub_rtsp_pull`

**步骤：**

```bash
# 终端 2：RTSP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/src_rtsp

# 终端 3：API 拉流
curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtsp://127.0.0.1:554/src_rtsp","stream_id":"pub_rtsp_pull"}'

# 终端 4：RTMP 播放
ffplay rtmp://127.0.0.1:1935/live/pub_rtsp_pull
```

**预期：** RTMP 播放正常。

---

## TC-21  RTSP API 拉流 → HTTP-FLV 播放

**说明：** RTSP 拉流后 HTTP-FLV 播放。

**stream_id：** 源 `src_rtsp`，拉流后 `pub_rtsp_pull`

**步骤：**

```bash
# 终端 2：RTSP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/src_rtsp

# 终端 3：API 拉流
curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtsp://127.0.0.1:554/src_rtsp","stream_id":"pub_rtsp_pull"}'

# 终端 4：FLV 播放
ffplay http://127.0.0.1:8081/flv/pub_rtsp_pull
```

**预期：** FLV 播放正常。

---

## TC-22  RTSP API 拉流 → HLS 播放

**说明：** RTSP 拉流后 HLS 播放。

**stream_id：** 源 `src_rtsp`，拉流后 `pub_rtsp_pull`

**步骤：**

```bash
# 终端 2：RTSP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/src_rtsp

# 终端 3：API 拉流
curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtsp://127.0.0.1:554/src_rtsp","stream_id":"pub_rtsp_pull"}'

# 终端 4：HLS 播放
ffplay http://127.0.0.1:8081/hls/pub_rtsp_pull/live.m3u8
```

**预期：** HLS 播放正常。

---

## TC-23  RTSP API 拉流 → WebRTC 播放

**说明：** RTSP 拉流后浏览器 WebRTC 播放。

**stream_id：** 源 `src_rtsp`，拉流后 `pub_rtsp_pull`

**步骤：**

```bash
# 终端 2：RTSP 源流
ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -g 25 -pix_fmt yuv420p \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/src_rtsp

# 终端 3：API 拉流
curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtsp://127.0.0.1:554/src_rtsp","stream_id":"pub_rtsp_pull"}'
```

1. 浏览器打开 `http://127.0.0.1:8081/webrtc/webrtc-test.html`
2. `stream_id` 填 `pub_rtsp_pull`，连接信令 → 开始播放

**预期：** 浏览器有画面。

---

## TC-24  WebRTC 推流 → RTMP 播放

**说明：** 浏览器 WebRTC 发布，RTMP 客户端播放。

**stream_id：** `pub_webrtc`

**步骤：**

1. 浏览器打开 `http://127.0.0.1:8081/webrtc/webrtc-test.html`
2. `stream_id` 填 `pub_webrtc`，点击「连接信令」→「开始推流」，允许摄像头/麦克风

```bash
# 终端 2：RTMP 播放
ffplay rtmp://127.0.0.1:1935/live/pub_webrtc
```

**预期：** ffplay 有画面；日志 `First published video frame`。

---

## TC-25  WebRTC 推流 → RTSP 播放

**说明：** 浏览器 WebRTC 发布，RTSP 播放。

**stream_id：** `pub_webrtc`

**步骤：**

1. 浏览器打开 `http://127.0.0.1:8081/webrtc/webrtc-test.html`
2. `stream_id` 填 `pub_webrtc`，连接信令 → 开始推流，允许摄像头/麦克风

```bash
# 终端 2：RTSP 播放
ffplay -rtsp_transport tcp rtsp://127.0.0.1:554/pub_webrtc
```

**预期：** RTSP 播放正常。

---

## TC-26  WebRTC 推流 → HTTP-FLV 播放

**说明：** 浏览器 WebRTC 发布，HTTP-FLV 播放。

**stream_id：** `pub_webrtc`

**步骤：**

1. 浏览器打开 `http://127.0.0.1:8081/webrtc/webrtc-test.html`
2. `stream_id` 填 `pub_webrtc`，连接信令 → 开始推流，允许摄像头/麦克风

```bash
# 终端 2：FLV 播放
ffplay http://127.0.0.1:8081/flv/pub_webrtc
```

**预期：** FLV 播放正常。

---

## TC-27  WebRTC 推流 → HLS 播放

**说明：** 浏览器 WebRTC 发布，HLS 播放。

**stream_id：** `pub_webrtc`

**步骤：**

1. 浏览器打开 `http://127.0.0.1:8081/webrtc/webrtc-test.html`
2. `stream_id` 填 `pub_webrtc`，连接信令 → 开始推流，允许摄像头/麦克风

```bash
# 终端 2：HLS 播放
ffplay http://127.0.0.1:8081/hls/pub_webrtc/live.m3u8
```

**预期：** HLS 切片与播放正常。

---

# 附录：无界面批量验证

任一项用例发布完成后，可将 `STREAM` 替换为对应 `stream_id`：

```bash
STREAM=pub_rtmp

ffmpeg -i rtmp://127.0.0.1:1935/live/$STREAM -t 5 -f null -
ffmpeg -rtsp_transport tcp -i rtsp://127.0.0.1:554/$STREAM -t 5 -f null -
ffmpeg -i http://127.0.0.1:8081/flv/$STREAM -t 5 -f null -
ffmpeg -i http://127.0.0.1:8081/hls/$STREAM/live.m3u8 -t 5 -f null -
```

# 附录：常见问题

| 现象 | 处理 |
|------|------|
| RTMP StreamNotFound | 检查 app/stream_name；播放须晚于推流 |
| RTSP 404 | 流未 publish；DESCRIBE 前须先推流或拉流成功 |
| UDP 无画面 | 拉流 URL 加 `?transport=udp`；确认服务端 H264 FU-A 分片 |
| HLS 无切片 | 先推流再请求 m3u8；检查 `config.toml` `[hls] enabled` |
| WebRTC 黑屏 | `stream_id` 一致；等待 IDR 关键帧 |
| 端口占用 | `lsof -i :1935` / `:554` / `:8081` / `:9080` |
