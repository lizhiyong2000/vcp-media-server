use crate::common::define::{ StreamTransmitEventReceiver};
use crate::manager::message;
use crate::manager::message::{EventKind, StreamTransmitEvent};
use crate::transmitter::source::StreamSource;
use crate::transmitter::StreamTransmitError;
use async_trait::async_trait;
use futures::lock::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use log::info;
use tokio::sync::broadcast;
use vcp_media_common::media::{FrameData, FrameDataReceiver, FrameDataSender};

pub struct RtspPushSource {
    stream_id: String,
    data_receiver: FrameDataReceiver,
    event_receiver: StreamTransmitEventReceiver,
    exit: broadcast::Sender<()>,
    frame_senders: Arc<Mutex<HashMap<String, FrameDataSender>>>,
}

#[async_trait]
impl StreamSource for RtspPushSource {
    async fn run(&mut self) -> Result<(), StreamTransmitError> {

        // self.receive_data_loop(tx.subscribe()).await;
        // self.receive_event_loop(tx).await;

        let mut receiver = self.exit.subscribe();

        loop {
            // info!("rtsp push source loop");
            tokio::select! {
                data = self.data_receiver.recv() => {


                    self.receive_frame_data(data).await;
                }

                event = self.event_receiver.recv() =>{

                    self.receive_event(event).await;
                }
                _ = receiver.recv()=>{
                info!("rtsp exit event received");
                    break;
                }
            }
        }


        Ok(())
    }
}


impl RtspPushSource {
    pub fn new(stream_id: String, data_receiver: FrameDataReceiver, event_receiver: StreamTransmitEventReceiver) -> Self {
        let (tx, _) = broadcast::channel::<()>(1);
        Self {
            stream_id,
            data_receiver,
            event_receiver,
            exit: tx,
            frame_senders: Arc::new(Default::default()),
        }
    }


    // async fn receive_data_loop(&mut self, mut exit: broadcast::Receiver<()>) {
    //     tokio::spawn(async move {
    //         loop {
    //             tokio::select! {
    //                 data = self.data_receiver.recv() => {
    //                    Self::receive_frame_data(data, &self.frame_senders).await;
    //                 }
    //                 _ = exit.recv()=>{
    //                     break;
    //                 }
    //             }
    //         }
    //     });
    // }

    async fn receive_event(&mut self, event: Option<StreamTransmitEvent>) {
        if let Some(event) = event {

            info!("rtsp event received");

            match event {
                StreamTransmitEvent::Subscribe(info)
                => {
                    // if let Err(err) = stream_handler
                    //     .send_prior_data(sender.clone(), info.sub_type)
                    //     .await
                    // {
                    //     log::error!("receive_event_loop send_prior_data err: {}", err);
                    //     break;
                    // }
                    // match sender {
                    //     DataSender::Frame {
                    //         sender: frame_sender,
                    //     } => {
                    //         frame_senders.lock().await.insert(info.id, frame_sender);
                    //     }
                    //     DataSender::Packet {
                    //         sender: packet_sender,
                    //     } => {
                    //         packet_senders.lock().await.insert(info.id, packet_sender);
                    //     }
                    // }
                    //
                    // if let Err(err) = result_sender.send(statistic_sender.clone()) {
                    //     log::error!(
                    //         "receive_event_loop:send statistic send err :{:?} ",
                    //         err
                    //     )
                    // }
                    //
                    // let mut statistics_data = statistics_data.lock().await;
                    // statistics_data.subscriber_count += 1;
                }
                StreamTransmitEvent::UnSubscribe(info) => {
                    // match info.sub_type {
                    //     SubscribeType::RtpPull | SubscribeType::WhepPull => {
                    //         packet_senders.lock().await.remove(&info.id);
                    //     }
                    //     _ => {
                    //         frame_senders.lock().await.remove(&info.id);
                    //     }
                    // }
                    // let mut statistics_data = statistics_data.lock().await;
                    // let subscribers = &mut statistics_data.subscribers;
                    // subscribers.remove(&info.id);
                    //
                    // statistics_data.subscriber_count -= 1;
                }
                StreamTransmitEvent::UnPublish => {
                    if let Err(err) = self.exit.send(()) {
                        log::error!("TransmitterEvent::UnPublish send error: {}", err);
                    }
                }
                // TransceiverEvent::Api { sender, uuid } => {
                // log::info!("api:  stream identifier: {:?}", uuid);
                // let statistic_data = if let Some(uid) = uuid {
                //     statistics_data.lock().await.query_by_uuid(uid)
                // } else {
                //     log::info!("api2:  stream identifier: {:?}", statistics_data);
                //     statistics_data.lock().await.clone()
                // };
                //
                // if let Err(err) = sender.send(statistic_data) {
                //     log::info!("Transmitter send avstatistic data err: {}", err);
                // }
                // }
                // TransceiverEvent::Request { sender } => {
                //     // stream_handler.send_information(sender).await;
                // }
            }
        }
    }


    async fn receive_frame_data(&mut self, data: Option<FrameData>) {
        if let Some(val) = data {
            match val {
                FrameData::MetaData {
                    timestamp: _,
                    data: _,
                } => {}
                FrameData::Audio { timestamp, data } => {
                    info!("rtsp audio frame data received");
                    let data = FrameData::Audio {
                        timestamp,
                        data: data.clone(),
                    };

                    for (_, v) in self.frame_senders.lock().await.iter() {
                        if let Err(audio_err) = v.send(data.clone()).map_err(|_| StreamTransmitError::SendAudioError) {
                            log::error!("Transmiter send error: {}", audio_err);
                        }
                    }
                }
                FrameData::Video { timestamp, data } => {
                    info!("rtsp video frame data received");
                    let data = FrameData::Video {
                        timestamp,
                        data: data.clone(),
                    };
                    for (_, v) in self.frame_senders.lock().await.iter() {
                        if let Err(video_err) = v.send(data.clone()).map_err(|_| StreamTransmitError::SendVideoError) {
                            log::error!("Transmiter send error: {}", video_err);
                        }
                    }
                }
                FrameData::MediaInfo {
                    media_info: info_value,
                } => {
                    let data = FrameData::MediaInfo {
                        media_info: info_value,
                    };
                    for (_, v) in self.frame_senders.lock().await.iter() {
                        if let Err(media_err) = v.send(data.clone()).map_err(|_| StreamTransmitError::SendMediaInfoError) {
                            log::error!("Transmiter send error: {}", media_err);
                        }
                    }
                }
            }
        }
    }
}