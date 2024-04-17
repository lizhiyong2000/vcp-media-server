use super::tcp_server;

use log::info;

pub struct RTMPServerSession{
    pub id: String,
}

impl tcp_server::TcpSession for RTMPServerSession{

    fn get_id(&self)->&String {
        return &self.id;
    }

    
    fn do_session(&self) {
        info!("RTMPServerSession");
    }
}