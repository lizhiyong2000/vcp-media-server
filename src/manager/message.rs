use std::fmt::{Debug, Write};
use tokio::sync::{mpsc, oneshot};

// 0.17.1
use tokio::sync::mpsc::UnboundedSender;
use vcp_media_common::media::{FrameDataReceiver, FrameDataSender, StreamInformation};
use vcp_media_sdp::SessionDescription;
use crate::common::stream::{PublishType, StreamId, SubscribeType};
// use tokio::sync::broadcast::error::SendError;
use crate::manager::stream_hub::StreamHubError;

// #[derive(Clone, Debug, EnumIter, Hash, Eq, PartialEq, Copy)]
// pub enum EventKind {
//     ApplicationEvent = 0,
//     ApiEvent = 1,
//     StreamHubEvent = 2,
//     StreamTransmitEvent = 3,
// }

// #[derive(Debug)]
// pub enum EventKindInfo {
//     ApplicationEventInfo(String),
//     ApiEventInfo(String),
//     StreamEventInfo{info:StreamHubEvent},
//     StreamTransmitEventInfo(StreamTransmitEvent),
// }
//
// #[derive(Debug)]
// pub struct Event {
//     pub kind: EventKind,
//     pub info: EventKindInfo,
// }

// impl From<EventKindInfo> for Event {
//     fn from(value: EventKindInfo) -> Self {
//         let event_kind = match value.clone() {
//             EventKindInfo::ApplicationEventInfo(value) => EventKind::ApplicationEventKind,
//             EventKindInfo::ApiEventInfo(value) => EventKind::ApiEventKind,
//             EventKindInfo::StreamEventInfo(value) => EventKind::StreamEventKind,
//             EventKindInfo::StreamTransmitEventInfo(value) => EventKind::StreamTransmitEventKind,
//         };
//
//         Event {
//             kind: event_kind,
//             info: value,
//         }
//     }
// }


// impl From<StreamHubEvent> for Event {
//     fn from(value: StreamHubEvent) -> Self {
//         Event {
//             kind: EventKind::StreamEventKind,
//             info: EventKindInfo::StreamEventInfo{info:value},
//         }
//     }
// }


#[derive(Clone, Debug)]
pub struct StreamPublishInfo {
    pub stream_id: StreamId,
    pub publish_type: PublishType,
    pub publisher_id: String,
}

#[derive(Clone, Debug)]
pub struct StreamSubscribeInfo {
    pub stream_id: StreamId,
    pub subscribe_type: SubscribeType,
    pub subscriber_id: String,
}

#[derive(Clone, Debug)]
pub struct StreamPullInfo {}



// impl Debug for PublishResultSender{
//     fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
//         todo!()
//     }
// }
// impl Clone for PublishResultSender{
//     fn clone(&self) -> Self {
//         todo!()
//     }
// }


// #[derive()]
pub enum StreamHubEvent {
    Publish{
        info:StreamPublishInfo,
        sdp:SessionDescription,
        receiver: FrameDataReceiver,
        result_sender:PublishResultSender,
    },
    UnPublish{
        info:StreamPublishInfo,
    },
    Subscribe{
        info:StreamSubscribeInfo,
        sender: FrameDataSender,
        result_sender:SubscribeResultSender,
    },
    UnSubscribe{
        info:StreamSubscribeInfo,
    },
    Request {
        stream_id: StreamId,
        result_sender: RequestResultSender,
    },
    // StartRelay(StreamPublishInfo),
    // StopRelay(StreamPullInfo),
}


pub type StreamHubEventSender = UnboundedSender<StreamHubEvent>;
pub type PublishResultSender = oneshot::Sender< Result<(), StreamHubError>>;
pub type SubscribeResultSender = oneshot::Sender< Result<(), StreamHubError>>;

pub type RequestResultSender = mpsc::UnboundedSender<StreamInformation>;


// impl Debug for StreamHubEvent {
//     fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
//         f.write_str("StreamHubEvent")
//         // format!(f ,"StreamHubEvent")
//     }
// }

// impl Clone for StreamHubEvent {
//     fn clone(&self) -> Self {
//         StreamHubEvent {}
//     }
// }


#[derive(Clone, Debug)]
pub enum StreamTransmitEvent {
    Subscribe {
        sender: FrameDataSender,
        info: StreamSubscribeInfo,
    },
    UnSubscribe{
        info:StreamSubscribeInfo
    },
    UnPublish{
        info:StreamPublishInfo
    },
    Request{
        sender: RequestResultSender,
    }
}



pub type StreamTransmitEventSender = mpsc::UnboundedSender<StreamTransmitEvent>;
pub type StreamTransmitEventReceiver = mpsc::UnboundedReceiver<StreamTransmitEvent>;

