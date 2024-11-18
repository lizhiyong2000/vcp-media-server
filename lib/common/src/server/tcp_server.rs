use std::collections::HashMap;
use std::sync::Arc;
use log::{error, info};
use tokio::net::TcpListener;
use crate::server::{NetworkServer, NetworkSession, SessionAlloc, SessionError, ServerSessionHandler, SessionManager, TcpSession, TcpServerHandler};

use std::marker::PhantomData;
use async_trait::async_trait;

pub struct TcpServer<T>
where T: TcpSession + 'static
{

    socket_addr: String,
    sessions: HashMap<String, Arc<Box<T>>>,
    session_alloc: SessionAlloc<T>,
    phantom: PhantomData<T>,
    connection_handler: Option<Box<dyn TcpServerHandler<T>>>,
    // bytes_pending: u64,
    // bytes_sent: u64,
    // bytes_received: u64,
}

impl<'a, T> SessionManager<T> for TcpServer<T>
where T: TcpSession
{
    fn get_session(&self, key: &str) -> Box<T> {
        todo!()
    }

    fn add_session(&self, session: Box<T>) -> Result<bool, SessionError> {
        todo!()
    }

    fn del_session(&self, session: Box<T>) -> Result<bool, SessionError> {
        todo!()
    }
}


impl<T>  TcpServer<T>
where T: TcpSession + 'static
{
    pub fn session_type(&self) -> String {
        return T::session_type();
    }

    pub async fn notify_session_created(&self, session: Arc<Box<T>>){

        // session.session_type()

    }
}

#[async_trait]
impl<'a, T> NetworkServer<'a, T> for TcpServer<T>
where T: TcpSession + 'static
{
    fn new(address: String, handler: Option<Box<dyn TcpServerHandler<T>>>) -> Self {
        let server = TcpServer {
            socket_addr: address,
            sessions: HashMap::new(),
            session_alloc: Default::default(),
            phantom: Default::default(),
            connection_handler: handler,
        };
        return server;
    }


    async fn start(&mut self) -> Result<(), SessionError> {
        let listener = TcpListener::bind(self.socket_addr.clone()).await?;
        info!("{} server started listen at:{}", self.session_type(), self.socket_addr);

        loop {
            let (socket, remote_addr) = listener.accept().await?;

            if let Some(handler) = self.connection_handler.as_mut(){
                if let  Ok(mut session) = handler.on_create_session(socket, remote_addr).await{
                    info!("server received connection from :{}, session id:{}", remote_addr, session.id());
                    // self.notify_session_created(session.clone()).await;
                    // session.set_handler()

                    tokio::spawn(
                        async move {

                            session.run().await;
                            info!("server end connection from :{}, session id:{}", remote_addr, session.id());
                        }
                    );
                }
            }else {
                if let mut session = self.session_alloc.new_tcp_session(socket, remote_addr){
                    info!("server received connection from :{}, session id:{}", remote_addr, session.id());
                    // self.notify_session_created(session.clone()).await;
                    // session.set_handler()

                    tokio::spawn(
                        async move {

                            session.run().await;
                            info!("server end connection from :{}, session id:{}", remote_addr, session.id());
                        }
                    );
                }
            }



        }
    }



}
