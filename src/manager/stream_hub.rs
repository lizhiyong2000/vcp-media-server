use std::collections::HashMap;
use std::sync::Arc;
use crate::manager::message::{RequestResultSender, StreamHubEvent, StreamPublishInfo, StreamSubscribeInfo, StreamTransmitEvent, StreamTransmitEventSender};
use crate::transmitter::StreamTransmitter;
use log::info;
use thiserror::Error;
use tokio::sync::mpsc;
use vcp_media_common::media::{FrameDataReceiver, FrameDataSender};
use vcp_media_sdp::SessionDescription;
use crate::common::stream::{HandleStreamTransmit, PublishType, StreamId};

#[derive(Debug, Error)]
pub enum StreamHubError {
    #[error("StreamUnPublishError")]
    StreamUnPublishError,

    #[error("StreamUnSubscribeError")]
    StreamUnSubscribeError,


    #[error("SendTransmitRequestError")]
    SendTransmitRequestError,

    #[error("SendTransmitUnPublishError")]
    SendTransmitUnPublishError,

    #[error("SendTransmitUnSubscribeError")]
    SendTransmitUnSubscribeError,

    #[error("StreamUnSubscribeError")]
    SendTransmitPriorDataError,


}

pub struct StreamHub {
    event_sender: mpsc::UnboundedSender<StreamHubEvent>,
    event_receiver: mpsc::UnboundedReceiver<StreamHubEvent>,
    streams: HashMap<StreamId, StreamTransmitEventSender>,
}

impl StreamHub {
    pub fn new() -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        Self {
            event_sender,
            event_receiver,
            streams: Default::default(),
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

            match event {
                StreamHubEvent::Publish{info, sdp, receiver,  result_sender, stream_handler} => {
                    info!("[EVENT] [StreamHub:Publish]:{:?}", info);
                    // let (sender, receiver) =
                    //     mpsc::unbounded_channel::<FrameData>();
                    self.handle_publish(info, sdp, receiver, stream_handler).await;

                    let result = Ok(());

                    if result_sender.send(result).is_err() {
                        log::error!("send publish result error error: The receiver dropped.")
                    }
                }

                StreamHubEvent::UnPublish { info } => {
                    info!("=====[StreamHub] [UnPublish]:{:?}", info);
                    self.handle_unpublish(info);
                }

                StreamHubEvent::Subscribe{info, sender, result_sender}  => {
                    info!("=====[StreamHub] [Subscribe]:{:?}", info);
                    // let (sender, receiver) =
                    //     mpsc::unbounded_channel::<FrameData>();
                    self.handle_subscribe(info, sender);

                    let result = Ok(());

                    if result_sender.send(result).is_err() {
                        log::error!("send subscribe result error: The receiver dropped.")
                    }
                }


                StreamHubEvent::UnSubscribe { info } => {
                    info!("=====[StreamHub] [UnSubscribe]:{:?}", info);
                    self.handle_unsubscribe(info);
                }
                // StreamHubEvent::StreamPull(_) => {}
                StreamHubEvent::Request { stream_id, result_sender } => {
                    info!("=====[StreamHub] [Request]:{:?}", stream_id);
                    if let Err(err) = self.handle_request(stream_id.clone(),  result_sender) {
                        log::error!("stream hub handle request error: {}", err);
                    }
                }

            }
        }
    }




    //publish a stream
    async fn handle_publish(&mut self, info: StreamPublishInfo, sdp:SessionDescription, receiver: FrameDataReceiver, stream_handler: Arc<dyn HandleStreamTransmit>) -> Result<(), StreamHubError> {
        info!("stream hub handle publish:{:?}", info);

        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let transceiver =
            StreamTransmitter::new(info.stream_id.clone());

        // let statistic_data_sender = transceiver.get_statistics_data_sender();
        let identifier_clone = info.stream_id.clone();

        tokio::spawn(async move{
            if let Err(err) = transceiver.run(PublishType::Push, sdp, receiver, event_receiver, stream_handler).await {
                log::error!(
                "transceiver run error, identifier: {}, error: {}",
                identifier_clone,
                err);
            } else {
                log::info!("transceiver run success, identifier: {}", identifier_clone);
            }
        });

        //对 transmitter 进行控制
        self.streams.insert(info.stream_id, event_sender);

        Ok(())
    }

    fn handle_subscribe(&mut self, info: StreamSubscribeInfo, sender: FrameDataSender) -> Result<(), StreamHubError> {
        if let Some(producer) = self.streams.get_mut(&info.stream_id) {
            log::info!("subscribe:  stream identifier: {}", info.stream_id);
            // let (result_sender, result_receiver) = oneshot::channel();
            let event = StreamTransmitEvent::Subscribe {
                sender,
                info,
            };

            producer.send(event).map_err(|_| StreamHubError::SendTransmitRequestError)?;

            return Ok(());
        }

        return Err(StreamHubError::SendTransmitRequestError);
    }


    fn handle_unpublish(&mut self, info: StreamPublishInfo) -> Result<(), StreamHubError>{
        match self.streams.get_mut(&info.stream_id) {
            Some(event_sender) => {

                let event = StreamTransmitEvent::UnPublish{
                    info:info.clone()
                };
                event_sender.send(event).map_err(|_| StreamHubError ::SendTransmitUnPublishError)?;

                self.streams.remove(&info.stream_id);
            }

            None => {
                return Err(StreamHubError::StreamUnPublishError);
            }
        }
        Ok(())
    }

    fn handle_unsubscribe(&mut self, info: StreamSubscribeInfo) -> Result<(), StreamHubError>{
        match self.streams.get_mut(&info.stream_id) {
            Some(event_sender) => {
                let event = StreamTransmitEvent::UnSubscribe{
                    info
                };
                event_sender.send(event).map_err(|_| StreamHubError ::SendTransmitUnSubscribeError)?;
            }

            None => {
                return Err(StreamHubError::StreamUnPublishError);
            }
        }
        Ok(())
    }

    fn handle_request(
        &mut self,
        identifier: StreamId,
        sender: RequestResultSender,
    ) -> Result<(), StreamHubError> {
        if let Some(producer) = self.streams.get_mut(&identifier) {
            let event = StreamTransmitEvent::Request { sender };
            info!("Request:  stream identifier: {}", identifier);
            producer.send(event).map_err(|_| StreamHubError::SendTransmitRequestError)?;
        }
        Ok(())
    }

}