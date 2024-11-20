pub mod rtsp_push_source;

use crate::transmitter::StreamTransmitError;
use async_trait::async_trait;

#[async_trait]
pub trait StreamSource {
    async fn run(&mut self) -> Result<(), StreamTransmitError>;
    // fn new(stream_id:String, data_receiver: FrameDataReceiver, event_receiver: StreamTransmitEventReceiver) -> Self;
    //
    // async fn receive_data_loop(exit: broadcast::Receiver<()>, data_receiver: FrameDataReceiver, frame_senders:Arc<Mutex<HashMap<String, FrameDataSender>>>);
    //
    // async fn receive_event_loop(exit: broadcast::Sender<()>, receiver: StreamTransmitEventReceiver);
}