use async_trait::async_trait;
use bytes::BytesMut;
use crate::common::define::{FrameDataSender, PublishType, SubscriberInfo};


#[derive(Debug)]
pub enum TransceiverEvent {
    Subscribe {
        sender: FrameDataSender,
        info: SubscriberInfo,
        // result_sender: TransceiverEventExecuteResultSender,
    },
    UnSubscribe {
        info: SubscriberInfo,
    },
    UnPublish {},
    //
    // Api {
    //     sender: StatisticStreamSender,
    //     uuid: Option<Uuid>,
    // },
    // Request {
    //     sender: InformationSender,
    // },
}


#[async_trait]
pub trait StreamSource{
    async fn handle_input(&mut self);
    fn get_source_id(&self) -> String;
    fn get_source_type(&self) -> PublishType;

    fn detach_sink(&mut self, sink:&mut Box<dyn StreamSink>) -> String;

    fn has_sinks(&self) -> bool{return false;}

    async fn start(&mut self);

    async fn stop(&mut self);
}

#[async_trait]
pub trait StreamSink{
    fn send_data(&mut self, data: &BytesMut);
    async fn link_to_source(&mut self, sink:&mut Box<dyn StreamSource>);

    async fn handle_output(&mut self);
}

pub trait StreamFilter : StreamSource + StreamSink{

}