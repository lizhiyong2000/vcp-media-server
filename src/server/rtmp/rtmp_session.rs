use crate::server::tcp_server;
use async_trait::async_trait;
use log::info;

pub struct RTMPServerSession{
    pub id: String,
}

#[async_trait]
impl tcp_server::TcpSession for RTMPServerSession{

    fn get_id(&self)->&String {
        return &self.id;
    }
    
    async fn run(&mut self) {
        info!("RTMPServerSession");
    }
}