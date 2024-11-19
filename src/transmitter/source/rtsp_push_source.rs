use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use futures::lock::Mutex;
use tokio::sync::broadcast;
use vcp_media_common::media::FrameData;
use crate::common::define::{FrameDataReceiver, FrameDataSender, StreamTransmitEventReceiver};
use crate::manager::message_hub;
use crate::manager::message_hub::{EventKind, StreamTransmitEvent};
use crate::transmitter::source::StreamSource;
use crate::transmitter::StreamTransmitError;

pub struct RtspPushSource{
    stream_id: String,
    data_receiver:FrameDataReceiver,
    event_receiver: StreamTransmitEventReceiver,
    frame_senders: Arc<Mutex<HashMap<String, FrameDataSender>>>,
}

#[async_trait]
impl StreamSource for RtspPushSource{
}



impl RtspPushSource{
    pub fn new(stream_id: String, data_receiver: FrameDataReceiver, event_receiver:StreamTransmitEventReceiver) -> Self {
       Self{
           stream_id,
           data_receiver,
           event_receiver,
           frame_senders: Arc::new(Default::default()),
       }
    }

    async fn run(self){
        let (tx, _) = broadcast::channel::<()>(1);
        Self::receive_data_loop(tx.subscribe(), self.data_receiver, self.frame_senders.clone()).await;
        Self::receive_event_loop(tx, self.event_receiver).await;
    }


    async fn receive_data_loop(mut exit: broadcast::Receiver<()>,
                               mut data_receiver: FrameDataReceiver,
                               frame_senders:Arc<Mutex<HashMap<String, FrameDataSender>>>) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    data = data_receiver.recv() => {
                       Self::receive_frame_data(data, &frame_senders).await;
                    }
                    _ = exit.recv()=>{
                        break;
                    }
                }
            }
        });
    }

    async fn receive_event_loop(mut exit: broadcast::Sender<()>, mut receiver: StreamTransmitEventReceiver) {
        tokio::spawn(async move {
            loop {
                if let Some(val) = receiver.recv().await {
                    match val {
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
                            if let Err(err) = exit.send(()) {
                                log::error!("TransmitterEvent::UnPublish send error: {}", err);
                            }
                            break;
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
        });
    }


    async fn receive_frame_data(
        data: Option<FrameData>,
        frame_senders: &Arc<Mutex<HashMap<String, FrameDataSender>>>,
    ) {
        if let Some(val) = data {
            match val {
                FrameData::MetaData {
                    timestamp: _,
                    data: _,
                } => {}
                FrameData::Audio { timestamp, data } => {
                    let data = FrameData::Audio {
                        timestamp,
                        data: data.clone(),
                    };

                    for (_, v) in frame_senders.lock().await.iter() {
                        if let Err(audio_err) = v.send(data.clone()).map_err(|_| StreamTransmitError::SendAudioError) {
                            log::error!("Transmiter send error: {}", audio_err);
                        }
                    }
                }
                FrameData::Video { timestamp, data } => {
                    let data = FrameData::Video {
                        timestamp,
                        data: data.clone(),
                    };
                    for (_, v) in frame_senders.lock().await.iter() {
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
                    for (_, v) in frame_senders.lock().await.iter() {
                        if let Err(media_err) = v.send(data.clone()).map_err(|_| StreamTransmitError::SendMediaInfoError) {
                            log::error!("Transmiter send error: {}", media_err);
                        }
                    }
                }
            }
        }
    }

}