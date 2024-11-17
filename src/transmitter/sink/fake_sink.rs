use async_trait::async_trait;
use bytes::BytesMut;
use crate::transmitter::traits::{StreamSink, StreamSource};

pub(crate) struct FakeSink {
    
}

#[async_trait]
impl StreamSink for FakeSink {
    fn send_data(&mut self, data: &BytesMut) {
        todo!()
    }

    async fn link_to_source(&mut self, sink: &mut Box<dyn StreamSource>) {
        todo!()
    }

    async fn handle_output(&mut self) {
        todo!()
    }
}
impl FakeSink {
    
}