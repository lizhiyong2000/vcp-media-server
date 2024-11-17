use log::info;
use tokio::sync::broadcast::Receiver;
use tokio::sync::mpsc;
use crate::server::message_hub;
use crate::server::message_hub::{Event, EventKind, EventKindInfo, StreamEvent};

pub struct StreamHub {
    stream_event_receiver: Receiver<Event>,
}

impl StreamHub {

    pub fn new() -> Self {
        let receiver = message_hub::subscribe_to(EventKind::StreamEvent);
        Self{
            stream_event_receiver: receiver,
        }

    }
    pub async fn run(&mut self) {
        info!("Starting stream hub");
        self.event_loop().await;
    }

    pub async fn event_loop(&mut self) {
        while let Ok(Event{info:EventKindInfo::StreamEvent(event), .. }) = self.stream_event_receiver.recv().await {
            match event {
                StreamEvent::StreamPublish(info) => {
                    info!("Stream publish:{:?}", info);
                }
                StreamEvent::StreamSubscribe(_) => {}
                StreamEvent::StreamPull(_) => {}
            }
        }
    }
}