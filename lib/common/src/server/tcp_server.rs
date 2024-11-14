use std::collections::HashMap;
use std::sync::Arc;
use log::{error, info};
use tokio::net::TcpListener;
use crate::server::{NetworkServer, NetworkSession, SessionAlloc, SessionError, SessionManager, TcpSession};

use std::marker::PhantomData;
use async_trait::async_trait;

pub struct TcpServer<T>
where T: TcpSession + 'static
{

    socket_addr: String,
    sessions: HashMap<String, Arc<Box<T>>>,
    session_alloc: SessionAlloc<T>,
    phantom: PhantomData<T>,
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

#[async_trait]
impl<'a, T> NetworkServer<'a, T> for TcpServer<T>
where T: TcpSession + 'static
{
    fn new(address: String) -> Self {
        let server = TcpServer {
            socket_addr: address,
            sessions: HashMap::new(),
            session_alloc: Default::default(),
            phantom: Default::default(),
        };
        return server;
    }

    async fn start(&mut self) -> Result<(), SessionError> {
        let listener = TcpListener::bind(self.socket_addr.clone()).await?;

        info!("server started listen at:{}", self.socket_addr);

        loop {

            let (socket, remote_addr) = listener.accept().await?;

            // let session_id = self.gen_session_id(remote_addr);

            if let Some(mut session) = self.session_alloc.new_tcp_session(socket, remote_addr){
                info!("server received connection from :{}, session id:{}", remote_addr, session.id());
                tokio::spawn(
                    async move {
                        // let session = TcpSession::new(server, remote_addr, socket);
                        session.run().await;

                        info!("server end connection from :{}, session id:{}", remote_addr, session.id());
                    }
                );
            }

        }
    }

}