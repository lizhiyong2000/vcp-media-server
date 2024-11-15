
## RTSP推流
ffmpeg -re -i ./example/test.mp4 -c copy -rtsp_transport tcp -f rtsp rtsp://127.0.0.1:8554/stream


## RTMP推流
ffmpeg -re -i ./example/test.mp4 -c copy -f flv rtmp://127.0.0.1:1935/stream/1