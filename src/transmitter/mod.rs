use std::sync::Arc;
use thiserror::Error;

mod traits;
mod source;
mod sink;

use sink::fake_sink::FakeSink;
use crate::common::define::{FrameDataReceiver, PublishType, StreamTransmitEventReceiver};
// use crate::transmitter::traits::StreamSource;
use crate::transmitter::source::rtsp_push_source::RtspPushSource;
use crate::transmitter::source::StreamSource;

#[derive(Debug, Error)]
pub enum StreamTransmitError{
    #[error("transmitter send video data error.")]
    SendVideoError,

    #[error("transmitter send audio data error.")]
    SendAudioError,

    #[error("transmitter send media info error.")]
    SendMediaInfoError,
}

// pub enum StreaSendAudioErrormSourceType{
//     RtspPush,
//     RtspPull,
//     RtmpPush,
//     RtmpPull,
// }
pub struct StreamTransmitter {
    stream_id:String,
    source_type: PublishType,
    source_element: Box<dyn StreamSource>,
    default_sink: Arc<Box<FakeSink>>,
    
}

impl StreamTransmitter {
    pub fn new(stream_id:String, source_type: PublishType, data_receiver:FrameDataReceiver, event_receiver:StreamTransmitEventReceiver)->Self{
        let source = match source_type {
            // PublishType::RtmpPush => {
            //     // RtmpPushSource::new(stream_id.clone(), data_receiver);
            // }
            // PublishType::RtmpPull => {}
            PublishType::RtspPush => {
                RtspPushSource::new(stream_id.clone(), data_receiver, event_receiver)
            }
            // PublishType::RtspPull => {}
            // PublishType::WhipPush => {}
            // PublishType::WhepPull => {}
            // PublishType::RtpPush => {}
        };

        Self{
            stream_id,
            source_type,
            source_element: Box::new(source),
            default_sink: Arc::new(Box::new(FakeSink {})),
        }
    }
}