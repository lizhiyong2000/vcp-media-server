use std::net::SocketAddr;
use std::sync::Arc;
use async_trait::async_trait;
use bytes::BytesMut;
use log::{info, error};
use tokio::sync::{oneshot, Mutex};
use vcp_media_common::media::{FrameDataReceiver, FrameDataSender};
use vcp_media_common::server::{NetworkServer, ServerSessionType, SessionError, TcpServerHandler};
use vcp_media_common::server::tcp_server::TcpServer;
use vcp_media_common::uuid::{RandomDigitCount, Uuid};
use vcp_media_rtmp::session::cache::Cache;
use vcp_media_rtmp::session::cache::errors::CacheError;
use vcp_media_rtmp::session::server_session::{RtmpServerSession, RtmpServerSessionContext, RtmpServerSessionHandler};
use vcp_media_sdp::SessionDescription;
use crate::common::stream::{HandleStreamTransmit, PublishType, StreamId, SubscribeType};
use crate::manager::message::{StreamHubEvent, StreamHubEventSender, StreamPublishInfo, StreamSubscribeInfo};
use crate::manager::stream_hub::StreamHubError;

pub struct  RtmpStreamTransmitHandler{
    /*cache is used to save RTMP sequence/gops/meta data
    which needs to be send to client(player) */
    /*The cache will be used in different threads(save
    cache in one thread and send cache data to different clients
    in other threads) */
    pub cache: Mutex<Option<Cache>>,
}

impl RtmpStreamTransmitHandler {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(None),
        }
    }

    pub async fn set_cache(&self, cache: Cache) {
        *self.cache.lock().await = Some(cache);
    }

    pub async fn save_video_data(
        &self,
        chunk_body: &BytesMut,
        timestamp: u32,
    ) -> Result<(), CacheError> {
        if let Some(cache) = &mut *self.cache.lock().await {
            cache.save_video_data(chunk_body, timestamp).await?;
        }
        Ok(())
    }

    pub async fn save_audio_data(
        &self,
        chunk_body: &BytesMut,
        timestamp: u32,
    ) -> Result<(), CacheError> {
        if let Some(cache) = &mut *self.cache.lock().await {
            cache.save_audio_data(chunk_body, timestamp).await?;
        }
        Ok(())
    }

    pub async fn save_metadata(&self, chunk_body: &BytesMut, timestamp: u32) -> Result<(), CacheError> {
        if let Some(cache) = &mut *self.cache.lock().await {
            cache.save_metadata(chunk_body, timestamp);
        }

        Ok(())
    }
}

#[async_trait]
impl HandleStreamTransmit for RtmpStreamTransmitHandler {
    async fn send_prior_data(&self, sender: FrameDataSender, sub_type: SubscribeType) -> Result<(), StreamHubError> {

        if let Some(cache) = &mut *self.cache.lock().await {
            if let Some(meta_body_data) = cache.get_metadata() {
                info!("send_prior_data: meta_body_data: ");
                sender.send(meta_body_data).map_err(|_| StreamHubError::SendTransmitPriorDataError)?;
            }
            if let Some(audio_seq_data) = cache.get_audio_seq() {
                info!("send_prior_data: audio_seq_data: ",);
                sender.send(audio_seq_data).map_err(|_| StreamHubError::SendTransmitPriorDataError)?;
            }
            if let Some(video_seq_data) = cache.get_video_seq() {
                info!("send_prior_data: video_seq_data:");
                sender.send(video_seq_data).map_err(|_| StreamHubError::SendTransmitPriorDataError)?;
            }
            match sub_type {
                SubscribeType::Pull => {
                    if let Some(gops_data) = cache.get_gops_data() {
                        for gop in gops_data {
                            for channel_data in gop.get_frame_data() {
                                sender.send(channel_data).map_err(|_| StreamHubError::SendTransmitPriorDataError)?;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }
}

pub struct VcpRtmpServerSessionHandler {

    event_producer: StreamHubEventSender,
    publish_info: Option<StreamPublishInfo>,
    subscribe_info: Option<StreamSubscribeInfo>,
    transmit_handler: Arc<RtmpStreamTransmitHandler>,
}



impl VcpRtmpServerSessionHandler{
    pub fn new(event_producer: StreamHubEventSender) -> Self {

        VcpRtmpServerSessionHandler{
            event_producer,
            publish_info: None,
            subscribe_info: None,
            transmit_handler: Arc::new(RtmpStreamTransmitHandler::new()),
        }
    }
}

#[async_trait]
impl RtmpServerSessionHandler for VcpRtmpServerSessionHandler {

    async fn save_video_data(
        &self,
        chunk_body: &BytesMut,
        timestamp: u32,
    ) -> Result<(), CacheError> {
        self.transmit_handler.save_video_data(chunk_body, timestamp).await
    }

    async fn save_audio_data(
        &self,
        chunk_body: &BytesMut,
        timestamp: u32,
    ) -> Result<(), CacheError> {
        self.transmit_handler.save_audio_data(chunk_body, timestamp).await
    }

    async fn save_metadata(&self, chunk_body: &BytesMut, timestamp: u32) -> Result<(), CacheError> {
        self.transmit_handler.save_metadata(chunk_body, timestamp).await;
        Ok(())
    }

    async fn handle_publish(&mut self, ctx: &mut RtmpServerSessionContext, frame_receiver: FrameDataReceiver) {

        let (result_sender, result_receiver) = oneshot::channel();

        let publisher_info = StreamPublishInfo {
            stream_id: StreamId::Rtmp { path: ctx.request_url.clone()},
            publish_type: PublishType::Push,
            publisher_id: ctx.session_id.clone(),
        };

        self.publish_info = Some(publisher_info.clone());

        self.transmit_handler
            .set_cache(Cache::new(5))
            .await;

        let publish_event = StreamHubEvent::Publish{
            info:publisher_info,
            sdp: SessionDescription::default(),
            receiver:frame_receiver,
            result_sender,
            stream_handler: self.transmit_handler.clone(),
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

        info!("handle_session_end, session_id: {}, session_type:{:?}", ctx.session_id, ctx.session_type);

        match ctx.session_type {
            ServerSessionType::Pull => {

                info!("subscribed rtmp server session {} end", ctx.session_id);

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
