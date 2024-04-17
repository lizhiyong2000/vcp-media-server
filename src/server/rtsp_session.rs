use super::tcp_server;

use log::info;

pub struct RTSPServerSession{
    pub id: String,
}

impl tcp_server::TcpSession for RTSPServerSession{
    fn do_session(&self) {
        info!("RTSPServerSession");
    }
    
    fn get_id(&self)->&String {
        return &self.id;
    }
}