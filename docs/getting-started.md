# 快速开始

## 环境要求

- Rust 1.70+（`cargo build`）
- ffmpeg / ffplay（测试推流与播放）

## 编译与启动

```bash
cd /path/to/vcp-media-server
cargo build
./target/debug/vcp-media-server
```

调试日志：

```bash
RUST_LOG=debug cargo run
```

模块级日志在 `config.toml` 的 `[log.modules]` 中配置。

## 端口一览

| 服务 | 配置项 | 默认 |
|------|--------|------|
| RTMP | `[rtmp] port` | 1935 |
| RTSP | `[rtsp] port` | 554 |
| HTTP API / FLV / HLS | `[http] port` | 8081 |
| WebRTC 信令 | `[webrtc] port` | 9080 |

## 日志

- 路径：`./logs/media-server.log`（按日滚动）
- 实时查看：`tail -f logs/media-server.log.*`

## HTTP API 速查

```bash
curl http://127.0.0.1:8081/health
curl http://127.0.0.1:8081/api/streams

curl -X POST http://127.0.0.1:8081/api/rtsp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtsp://127.0.0.1:554/live?transport=udp","stream_id":"pull_test"}'

curl -X POST http://127.0.0.1:8081/api/rtmp/pull \
  -H 'Content-Type: application/json' \
  -d '{"url":"rtmp://127.0.0.1:1935/live/source1","stream_id":"pull_test"}'
```

完整测试步骤见 `docs/test-cases.md`。
