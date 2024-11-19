pub mod rtsp_push_source;

use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use futures::lock::Mutex;
use tokio::sync::broadcast;
use crate::common::define::{FrameDataReceiver, FrameDataSender, StreamTransmitEventReceiver};

#[async_trait]
pub trait StreamSource{
    // fn new(stream_id:String, data_receiver: FrameDataReceiver, event_receiver: StreamTransmitEventReceiver) -> Self;
    //
    // async fn receive_data_loop(exit: broadcast::Receiver<()>, data_receiver: FrameDataReceiver, frame_senders:Arc<Mutex<HashMap<String, FrameDataSender>>>);
    //
    // async fn receive_event_loop(exit: broadcast::Sender<()>, receiver: StreamTransmitEventReceiver);
}