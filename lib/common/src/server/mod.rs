pub mod tcp_server;

use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;
use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("the http request has no request line.")]
    SocketIOError(#[from] std::io::Error),
}

pub trait ServerSessionHandler{}


#[async_trait]
pub trait NetworkSession : Send + Sync{

    fn id(&self)->String;
    fn session_type(&self)->String;

    // fn set_handler(&mut self, handler: Box<dyn ServerSessionHandler>);

    async fn run(&mut self);
}


pub trait TcpSession: NetworkSession{
    // fn session_type()->String{
    //     return "TCP".to_string()
    // }
    fn from_tcp_socket(sock: tokio::net::TcpStream, remote: SocketAddr) -> Self;
}

#[async_trait]
pub trait ServerHandler :Send+Sync {
    async fn on_session_created(&mut self, session: &mut Box<dyn NetworkSession>) -> Result<(), SessionError>;
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
    pub fn new_tcp_session(&self, sock: tokio::net::TcpStream, remote: SocketAddr) -> Option<T>{
        return Some(T::from_tcp_socket(sock, remote))
    }
}

impl<T> SessionAlloc<T> where T: UdpSession{
    pub fn new_udp_session(&self, sock: tokio::net::UdpSocket, remote: SocketAddr) -> Option<T>{
        return Some(T::from_udp_socket(sock, remote))
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
    fn new(address: String, handler: Option<Box<dyn ServerHandler>>) -> Self;
    async fn start(&mut self) -> Result<(), SessionError>;

}

