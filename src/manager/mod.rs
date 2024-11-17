pub mod api;
pub mod service;

pub trait TStreamPublisher {
    
    async fn get_info();

    async fn on_event();
}



pub trait TStreamSubscriber {
    async fn get_info();
    async fn on_event();
}


pub trait TStreamTransmitter {

    async fn on_publish_frame();

    async fn on_event();
    
}