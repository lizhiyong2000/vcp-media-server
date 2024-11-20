use std::fmt::{Debug, Formatter, Write};
use indexmap::IndexMap;
use lazy_static::lazy_static;
use std::sync::Mutex;
use tokio::sync::{broadcast, mpsc, oneshot};

use strum::IntoEnumIterator;
// 0.17.1
use strum_macros::EnumIter;
use tokio::sync::mpsc::error::SendError;
use tokio::sync::mpsc::UnboundedSender;
use vcp_media_common::media::FrameDataSender;
// use tokio::sync::broadcast::error::SendError;
use crate::manager::stream_hub::StreamHubError;

#[derive(Clone, Debug, EnumIter, Hash, Eq, PartialEq, Copy)]
pub enum EventKind {
    ApplicationEvent = 0,
    ApiEvent = 1,
    StreamHubEvent = 2,
    StreamTransmitEvent = 3,
}

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
    pub stream_id: String,
    pub stream_type: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct StreamSubscribeInfo {}

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
        result_sender:PublishResultSender,
    },
    Subscribe(StreamSubscribeInfo),
    // StartRelay(StreamPublishInfo),
    // StopRelay(StreamPullInfo),
}


pub type StreamHubEventSender = UnboundedSender<StreamHubEvent>;
pub type PublishResultSender = oneshot::Sender< Result<FrameDataSender, StreamHubError> >;



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
    Subscribe(StreamSubscribeInfo),
    UnSubscribe(StreamSubscribeInfo),
    UnPublish,
}

// #[derive(Debug)]
// pub struct EventBus<T>  {
//     // kind: EventKind,
//     sender: mpsc::UnboundedSender<T>,
//     receiver: mpsc::UnboundedReceiver<T>,
// }
//
//
// impl<T> EventBus<T> {
//     pub fn new() -> Self {
//         let (sender, receiver) = mpsc::unbounded_channel();
//         EventBus {sender, receiver }
//     }
//
//     pub fn get_sender(&self) -> mpsc::UnboundedSender<T> {
//         self.sender.clone()
//     }
// }
//
// pub struct MessageHub {
//     stream_hub: EventBus<StreamHubEvent>,
//     // transmit: EventBus<StreamTransmitEvent>,
//     // msg_bus: EventBus<>
// }
//
// impl MessageHub {
//     pub fn new() -> Self {
//         // let mut msg_buses: IndexMap<EventKind, EventBus> = IndexMap::new();
//         //
//         // for event_kind in EventKind::iter() {
//         //     msg_buses.insert(event_kind, EventBus::new(event_kind));
//         // }
//
//         Self { stream_hub: EventBus::new() }
//     }
//     pub fn subscribe_stream_hub(&self, event_kind: EventKind) -> mpsc::UnboundedSender<StreamHubEvent> {
//         // let msg_bus = self.msg_buses.get(&event_kind).unwrap();
//         self.stream_hub.receiver.
//     }
//
//     pub fn publish_stream_hub(&self, event: StreamHubEvent) -> Result<(), SendError<StreamHubEvent>> {
//         // let msg_bus = self.msg_buses.get(&event.kind).unwrap();
//         self.stream_hub.sender.send(event)
//     }
// }
// // // impl EventSender for MessageHub{
// // //     fn pub_event(&self) {
// // //         info!("MessageHub pub event");
// // //     }
// // //
// //
// lazy_static! {
//     static ref MESSSAG_HUB: Mutex<MessageHub> = Mutex::new(MessageHub::new());
// }
//
// pub fn subscribe_stream_hub(event_kind: EventKind) -> mpsc::UnboundedSender<StreamHubEvent> {
//     let msg_hub = MESSSAG_HUB.lock().unwrap();
//     msg_hub.subscribe_stream_hub(event_kind)
// }
// pub fn publish_stream_hub(event: StreamHubEvent) -> Result<(), SendError<StreamHubEvent>>{
//     let msg_hub = MESSSAG_HUB.lock().unwrap();
//     msg_hub.publish_stream_hub(event)
// }