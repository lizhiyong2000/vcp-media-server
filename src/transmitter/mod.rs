use std::sync::Arc;
use thiserror::Error;

mod traits;
mod source;
mod sink;

use crate::common::define::{ PublishType, StreamTransmitEventReceiver};
// use crate::transmitter::traits::StreamSource;
use crate::transmitter::source::rtsp_push_source::RtspPushSource;
use crate::transmitter::source::StreamSource;
use sink::fake_sink::FakeSink;
use vcp_media_common::media::FrameDataReceiver;
use vcp_media_sdp::SessionDescription;

#[derive(Debug, Error)]
pub enum StreamTransmitError {
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
    stream_id: String,
    // source_type: PublishType,
    // source_element: Box<dyn StreamSource>,
    default_sink: Arc<Box<FakeSink>>,

}

impl StreamTransmitter {
    pub fn new(stream_id: String) -> Self {
        // let source = match source_type {
        //     // PublishType::RtmpPush => {
        //     //     // RtmpPushSource::new(stream_id.clone(), data_receiver);
        //     // }
        //     // PublishType::RtmpPull => {}
        //     PublishType::RtspPush => {
        //         RtspPushSource::new(stream_id.clone(), data_receiver, event_receiver)
        //     }
        //     // PublishType::RtspPull => {}
        //     // PublishType::WhipPush => {}
        //     // PublishType::WhepPull => {}
        //     // PublishType::RtpPush => {}
        // };

        Self {
            stream_id,
            // source_type,
            // source_element: Box::new(source),
            default_sink: Arc::new(Box::new(FakeSink {})),
        }
    }

    pub async fn run(self, source_type: PublishType, sdp:SessionDescription, data_receiver: FrameDataReceiver, event_receiver: StreamTransmitEventReceiver) -> Result<(), StreamTransmitError> {
        let mut source = match source_type {
            // PublishType::RtmpPush => {
            //     // RtmpPushSource::new(stream_id.clone(), data_receiver);
            // }
            // PublishType::RtmpPull => {}
            PublishType::RtspPush => {
                RtspPushSource::new(self.stream_id.clone(), sdp, data_receiver, event_receiver)
            }
            // PublishType::RtspPull => {}
            // PublishType::WhipPush => {}
            // PublishType::WhepPull => {}
            // PublishType::RtpPush => {}
        };


        source.run().await
    }
}