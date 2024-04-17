use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use crate::common::Result;

use log::info;
use tokio::net::{TcpListener, TcpStream};

use super::{rtsp_session::RTSPServerSession, rtmp_session::RTMPServerSession, EventSender};


pub trait TcpSession : Send + Sync{

    fn get_id(&self)->&String; 
    fn do_session(&self);
}


pub enum ServerType {
    RTSP,
    RTMP,
    
}

// #[derive(Default)]
pub struct TcpServer {
    server_type: ServerType,
    socket_addr: String,
    sessions: HashMap<u64, Arc<Box<dyn TcpSession>>>,
    bytes_pending: u64,
    bytes_sent: u64,
    bytes_received: u64,

    event_pub: Arc<Box<dyn EventSender>>,
}


// impl  Send + Sync for struct TcpServer<'b> {
    
// }


impl TcpServer{
    pub fn new(
        stype: ServerType,
        address: String,
        enent_sender: Arc<Box<dyn EventSender>>,
    ) -> Self {


        return TcpServer{
            server_type: stype,
            socket_addr: address,
            sessions: HashMap::new(),
            bytes_pending:0,
            bytes_received:0,
            bytes_sent:0,
            event_pub:enent_sender,

        };

    }


    pub fn new_session(&self, id:String, remote:SocketAddr, stream: TcpStream) -> Box<dyn TcpSession>{
        match self.server_type {
            ServerType::RTSP => {
                return Box::new(RTSPServerSession{id});
            },
            ServerType::RTMP => {
                return Box::new(RTMPServerSession{id});
            },
        }
    }

    pub fn gen_session_id(&self, remote:SocketAddr)->String{
        return remote.to_string();
    }

    pub async fn start(&self) -> Result<()> {
        let listener = TcpListener::bind(self.socket_addr.clone()).await?;

        info!("server started listen at:{}", self.socket_addr);

        loop {
            
            let (socket, remote_addr) = listener.accept().await?;

            let session_id = self.gen_session_id(remote_addr);

            let session = self.new_session(session_id, remote_addr, socket);

            info!("server received connection from :{}, session id:{}", remote_addr, session.get_id());
            tokio::spawn(
                async move {
                    // let session = TcpSession::new(server, remote_addr, socket);
                    session.do_session();

                    info!("server end connection from :{}, session id:{}", remote_addr, session.get_id());
                }
            );


        }
    }
}