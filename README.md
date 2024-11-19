
## RTSP推流
ffmpeg -re -i ./example/test.mp4 -c copy -rtsp_transport tcp -f rtsp rtsp://127.0.0.1:8554/stream


## RTMP推流
ffmpeg -re -i ./example/test.mp4 -c copy -f flv rtmp://127.0.0.1:1935/stream/1



```rust
pub type OnFrameFn = Box<dyn Fn(FrameData) -> Result<(), UnPackerError> + Send + Sync>;

//Arc<Mutex<Box<dyn TNetIO + Send + Sync>>> : The network connection used by packer to send a/v data
//BytesMut: The Rtp packet data that will be sent using the TNetIO
pub type OnRtpPacketFn = Box<
    dyn Fn(
            Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>,
            RtpPacket,
        ) -> Pin<Box<dyn Future<Output = Result<(), PackerError>> + Send + 'static>>
        + Send
        + Sync,
>;

pub type OnRtpPacketFn2 =
    Box<dyn Fn(RtpPacket) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> + Send + Sync>;
```
