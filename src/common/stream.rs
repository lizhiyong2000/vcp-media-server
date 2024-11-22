/* Subscribe streams from stream hub */
use std::fmt;
use serde::Serialize;
#[derive(Debug, Serialize, Clone, PartialEq, Eq, Hash)]
pub enum StreamId{
    Rtsp{
        path:String,
    },
    Rtmp{
        path:String,
    },
}


impl fmt::Display for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            StreamId::Rtmp {
                path,
            } => {
                write!(f, "RTMP - path: {path}")
            }
            StreamId::Rtsp {
                path,
            } => {
                write!(f, "RTSP - path: {path}")
            }

        }
    }
}

#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub enum SubscribeType {
    Push,
    Pull,
}

/* Publish streams to stream hub */
#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub enum PublishType {
    Push,
    Pull,
}

#[derive(Debug, Serialize, Clone)]
pub struct NotifyInfo {
    pub request_url: String,
    pub remote_addr: String,
}


//we can only sub one kind of stream.
#[derive(Debug, Clone, Serialize)]
pub enum SubDataType {
    Frame,
    Packet,
}
//we can pub frame or packet or both.
#[derive(Debug, Clone, Serialize)]
pub enum PubDataType {
    Frame,
    Packet,
    Both,
}







