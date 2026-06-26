#!/bin/bash

# RTSP 推流和播放测试脚本（ffmpeg循环推流版本）
# 测试流程：启动媒体服务器 -> 启动循环RTSP推流 -> 启动ffplay播放 -> 验证播放

# 配置
MEDIA_SERVER_BIN="./target/release/media-server"
TEST_VIDEO="./test_video.mp4"
RTSP_URL="rtsp://localhost:554/live"
TEST_DURATION=30  # 播放持续时间（秒）
LOG_FILE="/tmp/rtsp_test.log"

echo "=========================================="
echo "RTSP 推流和播放测试"
echo "=========================================="
echo "视频文件: $TEST_VIDEO"
echo "推流地址: $RTSP_URL"
echo "播放时长: $TEST_DURATION 秒"
echo "=========================================="

# 清理之前的进程
echo "[1/5] 清理之前的进程..."
pkill -f "media-server" 2>/dev/null || true
pkill -f "ffmpeg -re" 2>/dev/null || true
pkill -f "ffplay" 2>/dev/null || true
sleep 1

# 编译媒体服务器
echo "[2/5] 编译媒体服务器..."
cargo build --release

# 启动媒体服务器
echo "[3/5] 启动媒体服务器..."
$MEDIA_SERVER_BIN > $LOG_FILE 2>&1 &
MEDIA_SERVER_PID=$!
echo "媒体服务器 PID: $MEDIA_SERVER_PID"

# 等待服务器启动
sleep 3

# 检查服务器是否启动成功
if ! pgrep -x "media-server" > /dev/null; then
    echo "ERROR: 媒体服务器启动失败"
    cat $LOG_FILE
    exit 1
fi
echo "媒体服务器启动成功"

# 启动循环RTSP推流（-stream_loop -1 表示无限循环播放源视频）
echo "[4/5] 启动循环RTSP推流..."
ffmpeg -re -stream_loop -1 -i $TEST_VIDEO -vcodec libx264 -preset ultrafast -tune zerolatency -f rtsp -rtsp_transport tcp $RTSP_URL > /tmp/ffmpeg_push.log 2>&1 &
FFMPEG_PID=$!
echo "ffmpeg 推流 PID: $FFMPEG_PID"

# 等待推流连接
sleep 3

# 检查推流是否成功
if ! pgrep -x "ffmpeg" > /dev/null; then
    echo "ERROR: ffmpeg 推流启动失败"
    cat /tmp/ffmpeg_push.log
    kill $MEDIA_SERVER_PID 2>/dev/null || true
    exit 1
fi
echo "RTSP 循环推流启动成功"

# 启动ffplay播放（使用timeout命令强制控制播放时长）
echo "[5/5] 启动ffplay播放（${TEST_DURATION}秒）..."
set +e  # 临时禁用 set -e，因为 timeout 超时会返回非零退出码
timeout ${TEST_DURATION}s ffplay -rtsp_transport tcp $RTSP_URL > /tmp/ffplay.log 2>&1
FFPLAY_EXIT_CODE=$?
set -e  # 重新启用 set -e

# timeout命令正常退出码为0，超时退出码为124
if [ $FFPLAY_EXIT_CODE -eq 124 ]; then
    echo "ffplay 播放超时（${TEST_DURATION}秒），正常退出"
    FFPLAY_EXIT_CODE=0
else
    echo "ffplay 退出码: $FFPLAY_EXIT_CODE"
fi

# 检查是否有错误
if grep -E "error|Error|ERROR" /tmp/ffplay.log > /dev/null; then
    echo "WARNING: ffplay 日志中存在错误信息"
    cat /tmp/ffplay.log
fi

# 清理进程
echo "清理进程..."
kill $FFMPEG_PID 2>/dev/null || true
kill $MEDIA_SERVER_PID 2>/dev/null || true
pkill -f "ffmpeg" 2>/dev/null || true
pkill -f "ffplay" 2>/dev/null || true

echo "=========================================="
if [ $FFPLAY_EXIT_CODE -eq 0 ]; then
    echo "测试成功！RTSP 循环推流和播放正常"
    exit 0
else
    echo "测试失败！ffplay 退出码: $FFPLAY_EXIT_CODE"
    exit 1
fi