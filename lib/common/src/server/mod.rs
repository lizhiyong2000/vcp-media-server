pub mod tcp_server;

use std::marker::PhantomData;
use std::net::SocketAddr;
use async_trait::async_trait;
use std::error;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("the http request has no request line.")]
    SocketIOError(#[from] std::io::Error),
}


#[async_trait]
pub trait NetworkSession : Send + Sync{

    fn id(&self)->String;
    fn session_type(&self)->String;

    async fn run(&mut self);
}


pub trait TcpSession: NetworkSession{
    fn session_type(&self)->String{
        return "TCP".to_string()
    }
    fn from_tcp_socket(sock: tokio::net::TcpStream) -> Self;
}

pub trait UdpSession: NetworkSession{
    fn session_type(&self)->String{
        return "UDP".to_string()
    }
    fn from_udp_socket(sock: tokio::net::UdpSocket) -> Self;
}





pub struct SessionAlloc<T> where T: NetworkSession{
    phantom: PhantomData<T>,
}

impl<T> SessionAlloc<T> where T: NetworkSession{
    // pub fn new() -> Self{
    //     return SessionAlloc
    // }
}

impl<T:NetworkSession> Default for SessionAlloc<T> {
    fn default() -> Self {
        return SessionAlloc{
            phantom: PhantomData
        }
    }
}


impl<T> SessionAlloc<T> where T: TcpSession{
    pub fn new_tcp_session(&self, sock: tokio::net::TcpStream, remote: SocketAddr) -> Option<T>{
        return Some(T::from_tcp_socket(sock))
    }
}

impl<T> SessionAlloc<T> where T: UdpSession{
    pub fn new_udp_session(&self, sock: tokio::net::UdpSocket) -> Option<T>{
        return Some(T::from_udp_socket(sock))
    }
}


pub trait SessionManager<SessionType> : Send + Sync
where SessionType: NetworkSession
{
    fn get_session(&self, key: &str) -> Box<SessionType>;
    fn add_session(&self, session: Box<SessionType>) -> Result<bool, SessionError>;
    fn del_session(&self, session: Box<SessionType>) -> Result<bool, SessionError>;
}

#[async_trait]
pub trait NetworkServer<'a, T> : SessionManager<T>
where T: TcpSession + 'static
{
    fn new(address: String) -> Self;
    async fn start(&mut self) -> Result<(), SessionError>;

}
