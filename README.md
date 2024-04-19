



ffmpeg -re -i ./example/test.mp4 -c copy -rtsp_transport tcp -f rtsp rtsp://127.0.0.1:9999/stream