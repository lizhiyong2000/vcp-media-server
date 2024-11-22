use crate::server::{NetworkServer, SessionAlloc, SessionError, SessionManager, TcpServerHandler, TcpSession};
use log::info;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;

use async_trait::async_trait;
use std::marker::PhantomData;

pub struct TcpServer<T>
where T: TcpSession + 'static
{

    socket_addr: String,
    sessions: HashMap<String, Arc<Box<T>>>,
    session_alloc: SessionAlloc<T>,
    phantom: PhantomData<T>,
    handler: Option<Box<dyn TcpServerHandler<T>>>,
    // bytes_pending: u64,
    // bytes_sent: u64,
    // bytes_received: u64,
}

impl<'a, T> SessionManager<T> for TcpServer<T>
where T: TcpSession
{
    fn get_session(&self, _key: &str) -> Box<T> {
        todo!()
    }

    fn add_session(&self, _session: Box<T>) -> Result<bool, SessionError> {
        todo!()
    }

    fn del_session(&self, _session: Box<T>) -> Result<bool, SessionError> {
        todo!()
    }
}


impl<T>  TcpServer<T>
where T: TcpSession + 'static
{
    pub fn session_type(&self) -> String {
        return T::session_type();
    }

    pub async fn notify_session_created(&self, _session: Arc<Box<T>>){

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
            handler,
        };
        return server;
    }


    async fn start(&mut self) -> Result<(), SessionError> {
        let listener = TcpListener::bind(self.socket_addr.clone()).await?;
        info!("{} server started listen at:{}", self.session_type(), self.socket_addr);

        loop {
            let (socket, remote_addr) = listener.accept().await?;

            if let Ok(mut session) = {
                if let Some(handler) = self.handler.as_mut() {
                    handler.on_create_session(socket, remote_addr).await
                }
                else {
                    Ok(self.session_alloc.new_tcp_session(socket, remote_addr))
                }
            }
            {
                info!("server received connection from :{}, session id:{}", remote_addr, session.id());
                tokio::spawn(
                    async move {
                        session.run().await;
                        info!("server end connection from :{}, session id:{}", remote_addr, session.id());
                        session.close().await;
                    }
                );
            }


        }
    }



}
