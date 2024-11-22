/* Subscribe streams from stream hub */
use serde::{Serialize, Serializer};
use serde::ser::SerializeStruct;
use tokio::sync::mpsc;
use crate::manager::message::StreamTransmitEvent;

#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub enum SubscribeType {
    /* Remote client request pulling(play) a rtmp stream.*/
    RtmpPull,
    /* Remote request to play httpflv triggers remux from RTMP to httpflv. */
    RtmpRemux2HttpFlv,
    /* The publishing of RTMP stream triggers remuxing from RTMP to HLS protocol.(NOTICE:It is not triggerred by players.)*/
    RtmpRemux2Hls,
    /* Relay(Push) local RTMP stream from stream hub to other RTMP nodes.*/
    RtmpRelay,
    /* Remote client request pulling(play) a rtsp stream.*/
    RtspPull,
    /* The publishing of RTSP stream triggers remuxing from RTSP to RTMP protocol.*/
    RtspRemux2Rtmp,
    /* Relay(Push) local RTSP stream to other RTSP nodes.*/
    RtspRelay,
    /* Remote client request pulling(play) stream through whep.*/
    WhepPull,
    /* Remuxing webrtc stream to RTMP */
    WebRTCRemux2Rtmp,
    /* Relay(Push) the local webRTC stream to other nodes using Whip.*/
    WhipRelay,
    /* Pull rtp stream by subscribing from stream hub.*/
    RtpPull,
}

/* Publish streams to stream hub */
#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub enum PublishType {
    // /* Receive rtmp stream from remote push client. */
    // RtmpPush,
    // /* Relay(Pull) remote RTMP stream to local stream hub. */
    // RtmpPull,
    /* Receive rtsp stream from remote push client */
    RtspPush,
    /* Relay(Pull) remote RTSP stream to local stream hub. */
    // RtspPull,
    // /* Receive whip stream from remote push client. */
    // WhipPush,
    // /* Relay(Pull) remote WebRTC stream to local stream hub using Whep. */
    // WhepPull,
    // /* It used for publishing raw rtp data of rtsp/whbrtc(whip) */
    // RtpPush,
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







