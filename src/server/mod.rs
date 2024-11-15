pub mod message_hub;



pub trait EventSender : Send + Sync{
    fn pub_event(&self);
}


