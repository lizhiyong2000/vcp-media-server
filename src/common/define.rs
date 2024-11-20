use crate::manager::message::StreamTransmitEvent;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
// use serde_derive::Serialize;
use tokio::sync::mpsc;
use vcp_media_common::media::FrameData;

pub mod http_method_name {
    pub const OPTIONS: &str = "OPTIONS";
    pub const PATCH: &str = "PATCH";
    pub const POST: &str = "POST";
    pub const DELETE: &str = "DELETE";
    pub const GET: &str = "GET";
}


/* Subscribe streams from stream hub */
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


#[derive(Debug, Clone)]
pub struct SubscriberInfo {
    pub id: String,
    pub sub_type: SubscribeType,
    // pub notify_info: NotifyInfo,
    pub sub_data_type: SubDataType,
}

impl Serialize for SubscriberInfo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // 3 is the number of fields in the struct.
        let mut state = serializer.serialize_struct("SubscriberInfo", 3)?;

        state.serialize_field("id", &self.id)?;
        state.serialize_field("sub_type", &self.sub_type)?;
        // state.serialize_field("notify_info", &self.notify_info)?;
        state.end()
    }
}

#[derive(Debug, Clone)]
pub struct PublisherInfo {
    pub id: String,
    pub pub_type: PublishType,
    pub pub_data_type: PubDataType,
    // pub notify_info: NotifyInfo,
}

impl Serialize for PublisherInfo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // 3 is the number of fields in the struct.
        let mut state = serializer.serialize_struct("PublisherInfo", 3)?;

        state.serialize_field("id", &self.id)?;
        state.serialize_field("pub_type", &self.pub_type)?;
        // state.serialize_field("notify_info", &self.notify_info)?;
        state.end()
    }
}




pub type StreamTransmitEventSender = mpsc::UnboundedSender<StreamTransmitEvent>;
pub type StreamTransmitEventReceiver = mpsc::UnboundedReceiver<StreamTransmitEvent>;
