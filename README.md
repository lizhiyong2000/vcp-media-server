
## RTSP推流
ffmpeg -re -stream_loop -1 -i ./example/test.mp4 -c copy -rtsp_transport tcp -f rtsp rtsp://127.0.0.1:8554/stream

ffmpeg -re -stream_loop -1 -i ./example/test.mp4 -c copy -rtsp_transport udp -f rtsp rtsp://127.0.0.1:8554/stream

## RTMP推流
ffmpeg -re -stream_loop -1 -i ./example/test.mp4 -c copy -f flv rtmp://127.0.0.1:1935/live/stream1


## ffplay播放
ffplay -rtsp_transport tcp rtsp://127.0.0.1:8554/stream