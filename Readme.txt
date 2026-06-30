================================================================================
  vcp-media-server 测试用例手册
================================================================================

已实现功能（概览）
--------------------------------------------------------------------------------
  协议 / 能力                          说明
  ───────────────────────────────────  ─────────────────────────────────────
  RTMP 推流 / 播放                     ffmpeg 等客户端 publish / play
  RTMP 拉流转发                        HTTP API 从远端 RTMP 拉流并本地 relay
  RTSP 推流 / 播放                     ANNOUNCE+RECORD 推流，DESCRIBE+PLAY 播放
  RTSP 拉流 / 推流                     HTTP API 从远端拉流或向远端推流
  HTTP-FLV 拉流                        /flv/<stream_id>，需先推流入库
  HLS 拉流                             /hls/<id>/live.m3u8，按需切片 MPEG-TS
  WebRTC 推流 / 播放                   WebSocket 信令 + 浏览器 H264 互通
  WebRTC 跨协议播放                    RTMP / RTSP 推入 → WebRTC 浏览器播放
  WebRTC 多页面同时播放                同一 stream_id 多播放器独立 relay
  WebRTC 同页推播                      单测试页可同时推流与播放（双 PC）
  流管理与 HTTP API                    /api/streams、拉流/推流控制接口
  内置 WebRTC 测试页                   http://127.0.0.1:8081/webrtc/webrtc-test.html

  核心机制：
    · 统一 StreamManager 广播，多协议写入、多协议读出
    · H264 Annex B / RTP 解析、SPS/PPS 提取与 fmtp 注入
    · WebRTC 播放低延迟：跳至 live 边缘、帧合并、IDR 起播
    · 推流端关键帧请求（need_keyframe → setParameters / generateKeyFrame）

本文档收录各协议推流 / 播放测试命令与脚本，按用例分节，便于后续扩展。

目录
  0. 通用说明 ........................ 启动、端口、日志
  1. RTMP 推流 / 拉流 ................ TC-RTMP-*
  2. RTSP 推流 / 播放 ................ TC-RTSP-*
  3. HTTP-FLV 拉流 ................... TC-FLV-*
  4. HLS 拉流 ........................ TC-HLS-*
  5. WebRTC .......................... TC-WEBRTC-*
  附录 A. 脚本命名规范
  附录 B. 通用日志检查


================================================================================
0. 通用说明
================================================================================

0.1 端口（config.toml）
--------------------------------------------------------------------------------
  协议        端口    地址示例
  RTMP        1935    rtmp://127.0.0.1:1935/...
  RTSP        554     rtsp://127.0.0.1:554/...
  HTTP        8081    http://127.0.0.1:8081/...
  WebRTC      9080    ws://127.0.0.1:9080/

0.2 启动服务
--------------------------------------------------------------------------------
cd /path/to/vcp-media-server
cargo run

# 全局调试
RUST_LOG=debug cargo run

# 按模块调试（config.toml [log.modules]）
  rtmp = "debug"
  rtsp = "debug"
  core = "debug"

0.3 日志
--------------------------------------------------------------------------------
日志路径：./logs/media-server.log.*

# 实时查看
tail -f logs/media-server.log.*

# 查看流列表（HTTP API）
curl http://127.0.0.1:8081/api/streams


================================================================================
1. RTMP 推流 / 拉流
================================================================================

用例编号    说明
TC-RTMP-01  ffmpeg 测试源推流 + ffplay 拉流（基础）
TC-RTMP-02  本地文件推流
TC-RTMP-03  ffmpeg 无界面拉流验证
TC-RTMP-04  限时推流（自动化测试）
TC-RTMP-05  RTMP 拉流转发 + ffplay 本地播放

----------------------------------------------------------------------------
1.1 URL 格式
----------------------------------------------------------------------------
  rtmp://<host>:<port>/<app>/<stream_name>

  示例：rtmp://127.0.0.1:1935/live/stream1
    live      → app（connect 参数）
    stream1   → stream_name（publish / play 参数）

  推流与拉流 stream_name 必须一致。

----------------------------------------------------------------------------
1.2 TC-RTMP-01  基础推流 + 播放
----------------------------------------------------------------------------
# 终端 1：启动服务
cargo run

# 终端 2：推流
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 30 \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/stream1

# 终端 3：拉流（推流开始后再执行）
ffplay -fflags nobuffer -flags low_delay -probesize 32 -analyzeduration 0 \
  rtmp://127.0.0.1:1935/live/stream1

----------------------------------------------------------------------------
1.3 TC-RTMP-02  本地文件推流
----------------------------------------------------------------------------
ffmpeg -re -i input.mp4 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 30 \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/stream1

----------------------------------------------------------------------------
1.4 TC-RTMP-03  无界面拉流验证
----------------------------------------------------------------------------
ffmpeg -i rtmp://127.0.0.1:1935/live/stream1 -t 5 -f null -

# 保存为文件
ffmpeg -i rtmp://127.0.0.1:1935/live/stream1 -t 30 -c copy output.flv

----------------------------------------------------------------------------
1.5 TC-RTMP-04  限时推流
----------------------------------------------------------------------------
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 30 \
  -c:a aac -ar 44100 -ac 2 -t 15 \
  -f flv rtmp://127.0.0.1:1935/live/stream1

----------------------------------------------------------------------------
1.6 TC-RTMP-05  RTMP 拉流转发（Pull + Relay）
----------------------------------------------------------------------------
从远端 RTMP 源拉流，转发到本地 stream_id，再通过本地 RTMP 服务播放。

# 终端 2：推流到 source1（模拟远端源）
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 30 \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/source1

# 终端 3：HTTP API 触发 RTMP 拉流（source1 -> pull_test）
curl -X POST http://127.0.0.1:8081/api/rtmp/pull \
  -H "Content-Type: application/json" \
  -d '{"url":"rtmp://127.0.0.1:1935/live/source1","stream_id":"pull_test"}'

# 终端 4：ffplay 播放转发后的本地流
ffplay rtmp://127.0.0.1:1935/live/pull_test

# 无界面验证
ffmpeg -i rtmp://127.0.0.1:1935/live/pull_test -t 5 -f null -

预期日志：
  [RTMP Puller] connect _result OK
  [RTMP Puller] createStream -> id=1
  [RTMP Puller] onStatus: NetStream.Play.Start
  [RTMP Puller] First relayed frame: codec=H264 ...
  [RTMP] >>> SEND ... frames (codec=H264 ...)  (本地 play)

----------------------------------------------------------------------------
1.7 脚本（scripts/rtmp/）
----------------------------------------------------------------------------
# test_rtmp_push.sh
#!/bin/bash
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 30 \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/stream1

# test_rtmp_play.sh
#!/bin/bash
ffplay -fflags nobuffer -flags low_delay \
  rtmp://127.0.0.1:1935/live/stream1

# test_rtmp_verify.sh
#!/bin/bash
sleep 3
ffmpeg -i rtmp://127.0.0.1:1935/live/stream1 -t 5 -f null - 2>&1 | tail -20

# test_rtmp_pull.sh — 触发 RTMP 拉流转发
#!/bin/bash
curl -X POST http://127.0.0.1:8081/api/rtmp/pull \
  -H "Content-Type: application/json" \
  -d "{\"url\":\"$1\",\"stream_id\":\"${2:-pull_test}\"}"

# test_rtmp_pull_play.sh — 播放转发流
#!/bin/bash
ffplay rtmp://127.0.0.1:1935/live/${1:-pull_test}

----------------------------------------------------------------------------
1.8 预期日志
----------------------------------------------------------------------------
推流：
  [RTMP] Publishing to stream: stream1
  [RTMP] <<< VIDEO AVC SequenceHeader (KeyFrame+H264)
  [RTMP] <<< AUDIO SequenceHeader format=AAC ...

拉流：
  [RTMP] --- play stream='stream1'
  [RTMP] Subscribed to stream 'stream1'
  [RTMP] >>> SEND first frame: codec=H264 ...
  [RTMP] >>> SEND 100 frames to player (codec=H264 ...)

----------------------------------------------------------------------------
1.9 常见问题
----------------------------------------------------------------------------
  无画面有声音   → 日志仅有 codec=AAC，检查 H264 NALU 是否发布
  StreamNotFound → stream_name 不一致，或拉流早于推流
  URL 错误       → 需包含 app：/live/stream1，不能写成 /stream1
  端口占用       → lsof -i :1935


================================================================================
2. RTSP 推流 / 播放
================================================================================

用例编号    说明
TC-RTSP-01  ffmpeg 测试源推流 + ffplay 播放（TCP，基础）
TC-RTSP-02  本地文件推流
TC-RTSP-03  ffmpeg 无界面播放验证
TC-RTSP-04  UDP 传输模式播放
TC-RTSP-05  HTTP API 从远端拉流（RTSP Pull）
TC-RTSP-06  HTTP API 向远端推流（RTSP Push）

----------------------------------------------------------------------------
2.1 URL 格式
----------------------------------------------------------------------------
  rtsp://<host>:<port>/<stream_id>

  示例：rtsp://127.0.0.1:554/stream1
    stream1   → 流 ID（路径最后一段）

  推流协议：ANNOUNCE → SETUP → RECORD
  播放协议：DESCRIBE → SETUP → PLAY

  建议使用 TCP 传输（-rtsp_transport tcp），与服务端 interleaved 模式兼容。

----------------------------------------------------------------------------
2.2 TC-RTSP-01  基础推流 + 播放
----------------------------------------------------------------------------
# 终端 1：启动服务
cargo run

# 终端 2：推流（ANNOUNCE + RECORD）
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 30 \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/stream1

# 终端 3：播放（DESCRIBE + SETUP + PLAY）
ffplay -rtsp_transport tcp -fflags nobuffer -flags low_delay \
  rtsp://127.0.0.1:554/stream1

----------------------------------------------------------------------------
2.3 TC-RTSP-02  本地文件推流
----------------------------------------------------------------------------
ffmpeg -re -i input.mp4 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 30 \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/stream1

----------------------------------------------------------------------------
2.4 TC-RTSP-03  无界面播放验证
----------------------------------------------------------------------------
ffmpeg -rtsp_transport tcp -i rtsp://127.0.0.1:554/stream1 -t 5 -f null -

# 保存为文件
ffmpeg -rtsp_transport tcp -i rtsp://127.0.0.1:554/stream1 -t 30 -c copy output.mp4

----------------------------------------------------------------------------
2.5 TC-RTSP-04  UDP 传输模式
----------------------------------------------------------------------------
# 推流（UDP）
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 30 \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport udp rtsp://127.0.0.1:554/stream1

# 播放（UDP）
ffplay -rtsp_transport udp rtsp://127.0.0.1:554/stream1

----------------------------------------------------------------------------
2.6 TC-RTSP-05  HTTP API 从远端拉流
----------------------------------------------------------------------------
# 服务端从远端 RTSP 源拉流，注册为本地 stream_id
curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H "Content-Type: application/json" \
  -d '{"url":"rtsp://192.168.1.100:554/live","stream_id":"pull_test"}'

# 本地播放拉取后的流
ffplay -rtsp_transport tcp rtsp://127.0.0.1:554/pull_test

----------------------------------------------------------------------------
2.7 TC-RTSP-06  HTTP API 向远端推流
----------------------------------------------------------------------------
# 先将流推入本服务（RTMP 或 RTSP），再转发到远端
curl -X POST http://127.0.0.1:8081/api/rtsp/push \
  -H "Content-Type: application/json" \
  -d '{"stream_id":"stream1","url":"rtsp://192.168.1.200:554/live"}'

----------------------------------------------------------------------------
2.8 脚本（scripts/rtsp/）
----------------------------------------------------------------------------
# test_rtsp_push.sh
#!/bin/bash
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 30 \
  -c:a aac -ar 44100 -ac 2 \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/stream1

# test_rtsp_play.sh
#!/bin/bash
ffplay -rtsp_transport tcp -fflags nobuffer -flags low_delay \
  rtsp://127.0.0.1:554/stream1

# test_rtsp_verify.sh
#!/bin/bash
sleep 3
ffmpeg -rtsp_transport tcp -i rtsp://127.0.0.1:554/stream1 -t 5 -f null - 2>&1 | tail -20

----------------------------------------------------------------------------
2.9 预期日志
----------------------------------------------------------------------------
推流（ANNOUNCE + RECORD）：
  [RTSP] Handling ANNOUNCE request
  [RTSP] ANNOUNCE stream_id=stream1
  [RTSP] ANNOUNCE parsed SDP: 2 tracks, SPS=true, PPS=true
  [RTSP] Handling RECORD request
  [RTSP] RECORD stream_id=stream1

播放（DESCRIBE + PLAY）：
  [RTSP] Handling DESCRIBE request
  [RTSP] DESCRIBE stream_id=stream1
  [RTSP] Handling PLAY request
  [RTSP] New connection from ...

----------------------------------------------------------------------------
2.10 常见问题
----------------------------------------------------------------------------
  404 Stream Not Found  → 拉流时流尚未 publish（需先推流）
  连接失败            → 检查 554 端口：lsof -i :554
  无画面              → 优先尝试 -rtsp_transport tcp
  DESCRIBE 失败       → 确认 stream_id 与推流路径一致


================================================================================
3. HTTP-FLV 拉流
================================================================================

用例编号    说明
TC-FLV-01   RTMP 推流后 HTTP-FLV 播放

----------------------------------------------------------------------------
3.1 URL 格式
----------------------------------------------------------------------------
  http://<host>:<port>/flv/<stream_id>

  示例：http://127.0.0.1:8081/flv/stream1

  需先通过 RTMP 或 RTSP 将流推入服务。

----------------------------------------------------------------------------
3.2 TC-FLV-01  推流 + HTTP-FLV 播放
----------------------------------------------------------------------------
# 终端 2：RTMP 推流（见 TC-RTMP-01）
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 30 \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/stream1

# 终端 3：HTTP-FLV 播放
ffplay http://127.0.0.1:8081/flv/stream1

----------------------------------------------------------------------------
3.3 脚本（scripts/flv/）
----------------------------------------------------------------------------
# test_flv_play.sh
#!/bin/bash
ffplay http://127.0.0.1:8081/flv/stream1


================================================================================
4. HLS 拉流
================================================================================

用例编号    说明
TC-HLS-01   RTMP 推流后 m3u8 播放

配置（config.toml [hls]）：
  enabled = true
  segment_duration = 4        # 目标切片时长（秒），在关键帧处切分
  max_segments = 10           # 播放列表保留的最大切片数
  output_dir = "./hls"        # 本地切片目录

----------------------------------------------------------------------------
4.1 URL 格式
----------------------------------------------------------------------------
  http://<host>:<port>/hls/<stream_id>/live.m3u8
  http://<host>:<port>/hls/<stream_id>/segment_<n>.ts

  示例：http://127.0.0.1:8081/hls/stream1/live.m3u8

  说明：
  - 首次请求 live.m3u8 会按需启动 HLS 切片（需已有推流）
  - 切片在关键帧 + 达到 segment_duration 时生成
  - 推流时建议 -g 与帧率匹配（如 25fps 用 -g 25），便于按时切分

----------------------------------------------------------------------------
4.2 TC-HLS-01  推流 + HLS 播放
----------------------------------------------------------------------------
# 终端 1：启动服务
cargo run

# 终端 2：RTMP 推流（关键帧间隔 1s，便于 4s 切片）
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=25 \
  -f lavfi -i sine=frequency=1000:sample_rate=44100 \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 25 \
  -c:a aac -ar 44100 -ac 2 \
  -f flv rtmp://127.0.0.1:1935/live/stream1

# 终端 3：HLS 播放（首次拉流约需 4–6 秒生成首片）
ffplay http://127.0.0.1:8081/hls/stream1/live.m3u8

# 无界面验证
ffmpeg -i http://127.0.0.1:8081/hls/stream1/live.m3u8 -t 5 -f null -

# 查看 m3u8
curl http://127.0.0.1:8081/hls/stream1/live.m3u8

预期：
  - m3u8 含 #EXTINF 与 segment_N.ts 条目
  - ./hls/stream1/ 下生成 segment_*.ts 与 live.m3u8
  - ffplay / ffmpeg 能正常解码播放


================================================================================
5. WebRTC 推流 / 播放
================================================================================

信令地址：ws://127.0.0.1:9080/
测试页面：http://127.0.0.1:8081/webrtc/webrtc-test.html

信令 JSON 格式（WebSocket 文本帧）：
  客户端 → 服务端
    {"type":"publish","stream_id":"<id>","sdp":"<offer sdp>"}
    {"type":"play","stream_id":"<id>","sdp":"<offer sdp>"}
    {"type":"ice","candidate":"...","sdp_mid":"0","sdp_mline_index":0}
  服务端 → 客户端
    {"type":"answer","sdp":"<answer sdp>"}
    {"type":"ice","candidate":"...","sdp_mid":"0","sdp_mline_index":0}
    {"type":"error","message":"..."}

用例编号    说明
TC-WEBRTC-01  浏览器 WebRTC 推流
TC-WEBRTC-02  浏览器 WebRTC 播放（播放 WebRTC 或 RTMP 推入的同一 stream_id）
TC-WEBRTC-03  RTMP 推流 + WebRTC 播放（跨协议）
TC-WEBRTC-04  RTSP 推流 + WebRTC 播放（跨协议）
TC-WEBRTC-05  RTSP 拉流 + WebRTC 播放（跨协议）

--------------------------------------------------------------------------------
TC-WEBRTC-01  浏览器 WebRTC 推流
--------------------------------------------------------------------------------

前置：服务已启动（cargo run）

1) 打开测试页
   http://127.0.0.1:8081/webrtc/webrtc-test.html

2) 点击「连接信令」，再点击「开始推流」，允许摄像头/麦克风权限。

3) 日志应出现：
   [WebRTC] Publish request stream='webrtc_test'
   [WebRTC] Publish session ready for stream 'webrtc_test'
   [WebRTC] First published video frame stream=webrtc_test

4) 验证流已注册：
   curl -s http://127.0.0.1:8081/api/streams | jq .

--------------------------------------------------------------------------------
TC-WEBRTC-02  浏览器 WebRTC 播放
--------------------------------------------------------------------------------

前置：TC-WEBRTC-01 已推流，或任意协议已向 stream_id 发布（如 RTMP live/stream1）

1) 新标签页打开测试页（或同一页先停止推流再播放）
2) stream_id 与推流一致，连接信令后点击「开始播放」
3) 远端 video 应出现画面；日志：
   [WebRTC] Play request stream='webrtc_test'
   [WebRTC] Subscribed to stream 'webrtc_test' for WebRTC play
   [WebRTC] First played video frame stream=webrtc_test

--------------------------------------------------------------------------------
TC-WEBRTC-03  RTMP 推流 + WebRTC 播放
--------------------------------------------------------------------------------

终端 A — RTMP 推流：
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=25 -pix_fmt yuv420p \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 50 \
  -f flv rtmp://127.0.0.1:1935/live/webrtc_bridge

浏览器 — WebRTC 播放：
  stream_id = webrtc_bridge
  http://127.0.0.1:8081/webrtc/webrtc-test.html → 连接 → 开始播放

日志关键字：
  grep -E "WebRTC|Publish|Play|First published|First played" logs/media-server.log.*

--------------------------------------------------------------------------------
TC-WEBRTC-04  RTSP 推流 + WebRTC 播放
--------------------------------------------------------------------------------

终端 A — RTSP 推流（ffmpeg ANNOUNCE/RECORD）：
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=25 -pix_fmt yuv420p \
  -c:v libx264 -preset ultrafast -tune zerolatency -g 50 -an \
  -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:554/live/rtsp_webrtc

浏览器 — WebRTC 播放：
  stream_id = rtsp_webrtc
  http://127.0.0.1:8081/webrtc/webrtc-test.html → 连接 → 开始播放

日志应出现：
  [RTSP-Push] First access unit stream='rtsp_webrtc' ...
  [WebRTC] Play streaming started stream='rtsp_webrtc' ...

--------------------------------------------------------------------------------
TC-WEBRTC-05  RTSP 拉流 + WebRTC 播放
--------------------------------------------------------------------------------

1) 先有一个可拉取的 RTSP 源（本地或其他 ffmpeg 推到的地址）
2) HTTP API 拉流到本地 stream_id：
curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H 'Content-Type: application/json' \
  -d '{"remote_url":"rtsp://127.0.0.1:554/live/rtsp_webrtc","local_stream_id":"rtsp_pull_play"}'

3) 浏览器 WebRTC 播放 stream_id = rtsp_pull_play

日志应出现：
  [RTSP Puller] SUCCESS: RTSP Pull started for stream rtsp_pull_play
  [RTSP-Pull] First access unit stream='rtsp_pull_play' ...
  [WebRTC] Play streaming started stream='rtsp_pull_play' ...


================================================================================
附录 A. 脚本命名规范
================================================================================

  scripts/
    rtmp/   test_rtmp_push.sh | test_rtmp_play.sh | test_rtmp_verify.sh | test_rtmp_pull.sh | test_rtmp_pull_play.sh
    rtsp/   test_rtsp_push.sh | test_rtsp_play.sh | test_rtsp_verify.sh
    flv/    test_flv_play.sh
    hls/    （待补充）
    webrtc/ webrtc-test.html（浏览器测试页，HTTP 8081 托管）

  命名规则：test_<协议>_<动作>.sh
    push   — 推流
    play   — 有界面播放
    verify — 无界面验证（ffmpeg -f null）


================================================================================
附录 B. 通用日志检查
================================================================================

# RTMP 关键字
grep -E "Publishing|AVC SequenceHeader|play stream=|SEND.*H264" logs/media-server.log.*

# RTSP 关键字
grep -E "ANNOUNCE|RECORD|DESCRIBE|PLAY|stream_id=" logs/media-server.log.*

# WebRTC 关键字
grep -E "WebRTC|Publish request|Play request|First published|First played" logs/media-server.log.*

# 错误
grep -E "StreamNotFound|not found|Connection error|failed" logs/media-server.log.*

================================================================================
