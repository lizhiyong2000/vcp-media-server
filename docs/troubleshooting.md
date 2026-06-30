# 故障排查

## 日志查看

```bash
tail -f logs/media-server.log.*

# RTMP
grep -E "Publishing|AVC SequenceHeader|play stream=|SEND.*H264" logs/media-server.log.*

# RTSP
grep -E "ANNOUNCE|RECORD|DESCRIBE|PLAY|Pull-UDP|First access" logs/media-server.log.*

# WebRTC
grep -E "WebRTC|Publish request|Play request|First published|First played" logs/media-server.log.*

# 错误
grep -Ei "StreamNotFound|not found|Connection error|failed" logs/media-server.log.*
```

## 端口占用

```bash
lsof -i :1935    # RTMP
lsof -i :554     # RTSP（macOS 可能与 AirPlay 冲突）
lsof -i :8081    # HTTP
lsof -i :9080    # WebRTC
```

## 常见问题

### RTMP 无画面

- 日志仅有 `codec=AAC`：检查 H264 NALU 是否发布
- `StreamNotFound`：确认 app/stream_name 与推流一致

### RTSP 404 / 无流

- DESCRIBE 前须已推流或拉流成功
- 优先使用 `-rtsp_transport tcp` 联调

### RTSP UDP 无画面

- 大帧须 FU-A 分片；确认服务端版本含 UDP 分片修复
- 拉流 URL 加 `?transport=udp`

### HTTP-FLV / HLS 无数据

- 须先有推流；HLS 需等待首片生成
- FLV：`curl -I http://127.0.0.1:8081/flv/<id>`

### WebRTC 黑屏

- 确认 `stream_id` 一致且已有 H264 关键帧
- 跨协议播放等待 IDR：`[WebRTC] Play streaming started`

## API 检查

```bash
curl -s http://127.0.0.1:8081/api/streams | jq .
curl -s http://127.0.0.1:8081/api/stream/live
```
