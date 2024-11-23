use std::net::SocketAddr;
use async_trait::async_trait;
use log::{info, error};
use tokio::sync::oneshot;
use vcp_media_common::media::{FrameDataReceiver, FrameDataSender};
use vcp_media_common::server::{NetworkServer, ServerSessionType, SessionError, TcpServerHandler};
use vcp_media_common::server::tcp_server::TcpServer;
use vcp_media_common::uuid::{RandomDigitCount, Uuid};
use vcp_media_rtmp::session::server_session::{RtmpServerSession, RtmpServerSessionContext, RtmpServerSessionHandler};
use vcp_media_sdp::SessionDescription;
use crate::common::stream::{PublishType, StreamId, SubscribeType};
use crate::manager::message::{StreamHubEvent, StreamHubEventSender, StreamPublishInfo, StreamSubscribeInfo};


pub struct VcpRtmpServerSessionHandler {

    event_producer: StreamHubEventSender,
    publish_info: Option<StreamPublishInfo>,
    subscribe_info: Option<StreamSubscribeInfo>,
}



impl VcpRtmpServerSessionHandler{
    pub fn new(event_producer: StreamHubEventSender) -> Self {

        VcpRtmpServerSessionHandler{
            event_producer,
            publish_info: None,
            subscribe_info: None,
        }
    }
}

#[async_trait]
impl RtmpServerSessionHandler for VcpRtmpServerSessionHandler {

    async fn handle_publish(&mut self, ctx: &mut RtmpServerSessionContext, frame_receiver: FrameDataReceiver) {

        let (result_sender, result_receiver) = oneshot::channel();

        let publisher_info = StreamPublishInfo {
            stream_id: StreamId::Rtmp { path: ctx.request_url.clone()},
            publish_type: PublishType::Push,
            publisher_id: ctx.session_id.clone(),
        };

        self.publish_info = Some(publisher_info.clone());

        let publish_event = StreamHubEvent::Publish{
            info:publisher_info,
            sdp: SessionDescription::default(),
            receiver:frame_receiver,
            result_sender,
        };

        self.event_producer.send(publish_event);

        let sender = match result_receiver.await {
            Ok(x) => {
                // self.frame_sender = Some(x);
                info!("rtmp server frame sender settled")
            },
            Err(_) => todo!(),
        };




    }

    async fn handle_play(&mut self, ctx: &mut RtmpServerSessionContext, frame_sender: FrameDataSender) {
        info!(
            "subscribe rtmp from stream_hub, url: {} subscribe_id: {}",
            ctx.request_url.clone(),
            ctx.session_id.clone()
        );

        let subscribe_info = StreamSubscribeInfo {
            stream_id: StreamId::Rtmp {
                path: ctx.request_url.clone(),
            },
            subscribe_type: SubscribeType::Pull,
            subscriber_id: ctx.session_id.clone(),
        };
        self.subscribe_info = Some(subscribe_info.clone());

        let (event_result_sender, event_result_receiver) = oneshot::channel();

        let subscribe_event = StreamHubEvent::Subscribe {
            info:  subscribe_info,
            sender: frame_sender,
            result_sender: event_result_sender,
        };
        let rv = self.event_producer.send(subscribe_event);

        if rv.is_err() {

            error!("publish rtmp event send failed: {:?}", rv);
            // return Err(SessionError::StreamHubEventSendErr);
        }


    }

    async fn handle_session_end(&mut self, ctx: &mut RtmpServerSessionContext) {
        match ctx.session_type {
            ServerSessionType::Pull => {

                if let Some(sub_info) = self.subscribe_info.as_mut() {
                    let unsubscribe_event = StreamHubEvent::UnSubscribe{
                        info: sub_info.clone(),
                    };

                    self.event_producer.send(unsubscribe_event);
                }
            }
            ServerSessionType::Push => {
                if let Some(pub_info) = self.publish_info.as_mut() {
                    let unpublish_event = StreamHubEvent::UnPublish{
                        info: pub_info.clone(),
                    };

                    self.event_producer.send(unpublish_event);
                }
            }
            ServerSessionType::Unknown => {}
        }
    }
}

pub struct RtmpServerHandler {
    hub_event_sender: StreamHubEventSender
}

impl RtmpServerHandler
{
    pub fn new(hub_event_sender: StreamHubEventSender) -> Self {
        Self {hub_event_sender}
    }
}

#[async_trait]
impl TcpServerHandler<RtmpServerSession> for crate::server::rtmp_server::RtmpServerHandler
{
    async fn on_create_session(&mut self, sock: tokio::net::TcpStream, remote: SocketAddr) -> Result<RtmpServerSession, SessionError> {
        // info!("Session {} created", session_id);
        let id = Uuid::new(RandomDigitCount::Zero).to_string();
        Ok(RtmpServerSession::new(id, sock, remote, Some(Box::new(VcpRtmpServerSessionHandler::new(self.hub_event_sender.clone())))))
    }

    async fn on_session_created(&mut self, session_id: String) -> Result<(), SessionError> {
        info!("Session {} created", session_id);
        Ok(())
    }
}




pub struct RtmpServer {
    tcp_server: TcpServer<RtmpServerSession>,
    // hub_event_sender: StreamHubEventSender,
}


impl RtmpServer {
    pub fn new(addr: String, hub_event_sender: StreamHubEventSender) -> Self {
        let server_handler = Box::new(
            crate::server::rtmp_server::RtmpServerHandler::new(hub_event_sender)
        );
        let rtmp_server: TcpServer<RtmpServerSession> = TcpServer::new(addr, Some(server_handler));

        let res = Self {
            tcp_server: rtmp_server,
            // hub_event_sender,
        };
        res
    }

    pub fn session_type(&self) -> String {
        self.tcp_server.session_type()
    }

    pub async fn start(&mut self) -> Result<(), SessionError> {
        self.tcp_server.start().await
    }
}
