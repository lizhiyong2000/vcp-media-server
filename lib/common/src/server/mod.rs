pub mod tcp_server;

use async_trait::async_trait;
use std::marker::PhantomData;
use std::net::SocketAddr;
use serde_derive::Serialize;
use thiserror::Error;

/* Subscribe streams from stream hub */
#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub enum ServerSessionType {
    Pull,
    Push,
    Unknown,
}

#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub enum ClientSessionType {
    Pull,
    Push,
}


#[derive(Debug, Error)]
pub enum SessionError {
    #[error("the http request has no request line.")]
    SocketIOError(#[from] std::io::Error),


    #[error("the http request has no request line.")]
    StreamHubEventSendErr,
}

pub trait ServerSessionHandler{

}


#[async_trait]
pub trait NetworkSession : Send + Sync{
    fn id(&self)->String;
    fn session_type()->String;
    async fn run(&mut self);
    async fn close(&mut self);
}


pub trait TcpSession: NetworkSession{
    // fn session_type()->String{
    //     return "TCP".to_string()
    // }
    fn from_tcp_socket(sock: tokio::net::TcpStream, remote: SocketAddr) -> Self;

    // fn notify_created(&self);
}

#[async_trait]
pub trait TcpServerHandler<SessionType>:Send+Sync
where SessionType:TcpSession{
    async fn on_create_session(&mut self, sock: tokio::net::TcpStream, remote: SocketAddr) -> Result<SessionType, SessionError>;
    async fn on_session_created(&mut self, session_id:String) -> Result<(), SessionError>;

}

pub trait UdpSession: NetworkSession{
    // fn session_type()->String{
    //     return "UDP".to_string()
    // }
    fn from_udp_socket(sock: tokio::net::UdpSocket, remote: SocketAddr) -> Self;
}





pub struct SessionAlloc<T> where T: NetworkSession{
    phantom: PhantomData<T>,
}

impl<T> SessionAlloc<T> where T: NetworkSession{
    // pub fn new() -> Self{
    //     return SessionAlloc
    // }
    
    // pub fn session_type(&self) -> String{
    //     return T::session_type();
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
    pub fn new_tcp_session(&self, sock: tokio::net::TcpStream, remote: SocketAddr) -> T{
        T::from_tcp_socket(sock, remote)
    }
}

impl<T> SessionAlloc<T> where T: UdpSession{
    pub fn new_udp_session(&self, sock: tokio::net::UdpSocket, remote: SocketAddr) -> T{
        T::from_udp_socket(sock, remote)
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
    fn new(address: String, handler: Option<Box<dyn TcpServerHandler<T>>>) -> Self;
    async fn start(&mut self) -> Result<(), SessionError>;

}

