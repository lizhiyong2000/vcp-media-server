use crate::common::define::{PublishType};
use crate::manager::message;
use crate::manager::message::{EventKind, StreamHubEvent, StreamPublishInfo};
use crate::transmitter::StreamTransmitter;
use log::info;
use std::sync::Arc;
use tokio::sync::mpsc;
use vcp_media_common::media::{FrameData, FrameDataReceiver};

pub enum StreamHubError {}

pub struct StreamHub {
    event_sender: mpsc::UnboundedSender<StreamHubEvent>,
    event_receiver: mpsc::UnboundedReceiver<StreamHubEvent>,
}

impl StreamHub {
    pub fn new() -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        Self {
            event_sender,
            event_receiver,
        }
    }
    pub async fn run(&mut self) {
        info!("Stream hub now working.");
        self.event_loop().await;
    }

    pub fn get_sender(&mut self) -> mpsc::UnboundedSender<StreamHubEvent> {
        self.event_sender.clone()
    }

    pub async fn event_loop(&mut self) {
        while let Some(event) = self.event_receiver.recv().await {
            // info!("[MESSAGE] [Stream Hub]:{:?}", event);
            match event {
                StreamHubEvent::Publish{info, result_sender} => {
                    info!("Stream publish:{:?}", info);
                    let (sender, receiver) =
                        mpsc::unbounded_channel::<FrameData>();
                    self.handle_publish(info, receiver).await;

                    let result = Ok(sender);

                    if result_sender.send(result).is_err() {
                        log::error!("event_loop Subscribe error: The receiver dropped.")
                    }
                }
                StreamHubEvent::Subscribe(_) => {}
                // StreamHubEvent::StreamPull(_) => {}
            }
        }
    }

    //publish a stream
    pub async fn handle_publish(&mut self, info: StreamPublishInfo, receiver: FrameDataReceiver) -> Result<(), StreamHubError> {
        info!("[MESSAGE] [Stream Hub] handle publish:{:?}", info);

        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let mut transceiver =
            StreamTransmitter::new(info.stream_id.clone());

        // let statistic_data_sender = transceiver.get_statistics_data_sender();
        let identifier_clone = info.stream_id.clone();

        tokio::spawn(async move{
            if let Err(err) = transceiver.run(PublishType::RtspPush, receiver, event_receiver).await {
                log::error!(
                "transceiver run error, identifier: {}, error: {}",
                identifier_clone,
                err,
            );
            } else {
                log::info!("transceiver run success, identifier: {}", identifier_clone);
            }
        });




        //对 transmitter 进行控制
        // self.streams.insert(identifier.clone(), event_sender);




        Ok(())
    }
}