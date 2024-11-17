use std::sync::Arc;

mod traits;
mod source;
mod sink;

use sink::fake_sink::FakeSink;
use crate::transmitter::traits::StreamSource;

pub enum StreamSourceType{
    RtspPush,
    RtspPull,
    RtmpPush,
    RtmpPull,
}
pub struct StreamTransmitter {
    stream_id:String,
    source_type: StreamSourceType,
    source_element: Arc<Box<dyn StreamSource>>,
    default_sink: Arc<Box<FakeSink>>,
    
}

impl StreamTransmitter {
    
}