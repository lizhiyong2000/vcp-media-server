use std::iter::Map;
use std::sync::Mutex;
use indexmap::IndexMap;
use log::info;
use lazy_static::lazy_static;
use tokio::sync::{broadcast};

use strum::IntoEnumIterator; // 0.17.1
use strum_macros::{Display, EnumIter};
use tokio::sync::broadcast::error::SendError;

#[derive(Clone, Debug, EnumIter, Hash, Eq, PartialEq, Copy)]
pub enum EventKind {
    ApplicationEvent = 0,
    ApiEvent = 1,
    StreamEvent = 2,
    StreamTransmitterEvent = 3,
}

#[derive(Clone, Debug)]
pub enum EventKindInfo {
    ApplicationEvent(String),
    ApiEvent(String),
    StreamEvent(StreamEvent),
    StreamTransmitterEvent(String),
}

#[derive(Clone, Debug)]
pub struct Event{
    pub kind: EventKind,
    pub info: EventKindInfo,
}


#[derive(Clone, Debug)]
pub struct StreamPublishInfo{
    pub stream_id: String,
    pub stream_type: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct StreamSubscribeInfo{

}

#[derive(Clone, Debug)]
pub struct StreamPullInfo{

}

#[derive(Clone, Debug)]
pub enum StreamEvent{
    StreamPublish(StreamPublishInfo),
    StreamSubscribe(StreamSubscribeInfo),
    StreamPull(StreamPullInfo),
}

#[derive(Debug)]
pub struct EventBus{
    kind: EventKind,
    sender: broadcast::Sender<Event>,
    receiver: broadcast::Receiver<Event>,
}


impl Clone for EventBus {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind.clone(),
            sender: self.sender.clone(),
            receiver: self.sender.subscribe(),
        }
    }
}


impl EventBus {
    pub fn new(kind:EventKind) -> Self {
        let (sender, receiver) = broadcast::channel(100);
        EventBus { kind, sender, receiver }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }
}

pub struct MessageHub{
    msg_buses:IndexMap<EventKind, EventBus>,
}

impl MessageHub {

    pub fn new() -> Self {

        let mut msg_buses:IndexMap<EventKind, EventBus> = IndexMap::new();

        for event_kind in EventKind::iter() {
            msg_buses.insert(event_kind, EventBus::new(event_kind));
        }


        Self{ msg_buses }
        
    }
    pub fn subscribe_to(&self, event_kind: EventKind) -> broadcast::Receiver<Event> {
        let msg_bus = self.msg_buses.get(&event_kind).unwrap();
        msg_bus.subscribe()
    }

    pub fn publish(&self, event: Event) -> Result<usize, SendError<Event>> {
        let msg_bus = self.msg_buses.get(&event.kind).unwrap();
        msg_bus.sender.send(event)
    }
}
// impl EventSender for MessageHub{
//     fn pub_event(&self) {
//         info!("MessageHub pub event");
//     }
//


lazy_static! {
    static ref MESSSAG_HUB: Mutex<MessageHub> = Mutex::new(MessageHub::new());
}

pub fn subscribe_to(event_kind: EventKind) -> broadcast::Receiver<Event> {
    let msg_hub = MESSSAG_HUB.lock().unwrap();
    msg_hub.subscribe_to(event_kind)
}
pub fn publish_event(event:Event) {
    let msg_hub = MESSSAG_HUB.lock().unwrap();
    msg_hub.publish(event);
}