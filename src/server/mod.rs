pub mod tcp_server;
mod rtmp_session;
mod rtsp_session;
pub mod message_hub;



pub trait EventSender : Send + Sync{
    fn pub_event(&self);
}


