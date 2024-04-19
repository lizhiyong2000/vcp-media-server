pub mod tcp_server;
pub mod rtmp;
pub mod rtsp;
pub mod message_hub;



pub trait EventSender : Send + Sync{
    fn pub_event(&self);
}


