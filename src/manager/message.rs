use crate::server::EventSender;
use log::info;

pub struct MessageHub{

}

impl EventSender for MessageHub{
    fn pub_event(&self) {
        info!("MessageHub pub event");
    }
}

impl MessageHub {
    pub fn new() ->Self{
        return MessageHub{};
    }
}