use async_trait::async_trait;
use log::info;
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot};
use vcp_media_common::server::tcp_server::TcpServer;
use vcp_media_common::Unmarshal;
use vcp_media_rtsp::session::errors::RtspSessionError;
use vcp_media_rtsp::session::server_session::{RtspServerSession, RtspServerSessionContext, HandleRtspServerSession};

use crate::manager::message::{StreamHubEvent, StreamHubEventSender, StreamPublishInfo, StreamSubscribeInfo};
use vcp_media_common::http::HttpRequest as RtspRequest;
use vcp_media_common::http::HttpResponse as RtspResponse;
use vcp_media_common::media::{FrameDataReceiver, FrameDataSender, StreamInformation};
use vcp_media_common::server::{NetworkServer, NetworkSession, ServerSessionType, SessionError, TcpServerHandler};
use vcp_media_common::uuid::{RandomDigitCount, Uuid};
use vcp_media_sdp::SessionDescription;
use crate::common::stream::{PublishType, StreamId, SubscribeType};

pub struct VcpRtspServerSessionHandler {
    // session: Option<Arc<Mutex<RTSPServerSession>>>,
    // tracks: HashMap<TrackType, RtspTrack>,
    sdp: SessionDescription,
    // session_id: Option<Uuid>,
    event_producer: StreamHubEventSender,

    publish_info: Option<StreamPublishInfo>,
    subscribe_info: Option<StreamSubscribeInfo>,
    // frame_sender: Option<FrameDataSender>,
    // frame_receiver: Option<FrameDataReceiver>,
}
impl VcpRtspServerSessionHandler {
    pub fn new(event_producer: StreamHubEventSender) -> Self {
        Self {
            event_producer,
            // frame_sender:None,
            // frame_receiver:None,
            sdp: SessionDescription::default(),
            subscribe_info:None,
            publish_info:None,
            // session:None,
            // tracks: HashMap::new(),
            // sdp: SessionDescription::default(),
            // session_id: None,
        }
    }

    // pub fn get_session_io(&mut self) -> Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>{
    //     self.session.lock().get_io()
    // }

    // pub async fn send_response(&mut self, response: &RtspResponse) -> Result<(), RtspSessionError> {
    //     // if let Some(session) = self.session.unwrap().lock();
    //     self.session.unwrap().lock().await.send_response(response).await
    // }

    pub fn unsubscribe_from_stream_hub(&mut self, _stream_path: String) -> Result<(), RtspSessionError> {
        // let identifier = StreamIdentifier::Rtsp { stream_path };

        // let subscribe_event = StreamHubEvent::UnSubscribe {
        //     identifier,
        //     info: self.get_subscriber_info(),
        // };
        // if let Err(err) = self.event_producer.send(subscribe_event) {
        //     log::error!("unsubscribe_from_stream_hub err {}", err);
        // }

        Ok(())
    }
}

#[async_trait]
impl HandleRtspServerSession for VcpRtspServerSessionHandler {
    // fn get_frame_sender(&mut self) -> Option<FrameDataSender> {
    //     return self.frame_sender.clone()
    // }
    //
    // fn get_frame_receiver(&mut self) -> Option<FrameDataReceiver> {
    //     return self.frame_receiver.clone()
    // }
    // async fn on_frame_data(&mut self, frame_data: FrameData) {
    //     info!("Received frame data from {:?}", frame_data);
    //
    //     if let Some(frame_sender) = self.frame_sender.as_mut() {
    //         frame_sender.send(frame_data);
    //     }
    // }

    async fn handle_close(&mut self, ctx: &mut RtspServerSessionContext) -> Result<(), RtspSessionError>{
        match ctx.session_type {
            ServerSessionType::Pull => {

                if let Some(sub_info) = self.subscribe_info.as_mut() {
                    let unpublish_event = StreamHubEvent::UnSubscribe{
                        info: sub_info.clone(),
                    };

                    self.event_producer.send(unpublish_event);
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
        Ok(())
    }

    async fn handle_rtp_over_rtsp_message(&mut self, ctx: &mut RtspServerSessionContext, channel_identifier: u8, length: usize) -> Result<(), RtspSessionError> {
        // let mut cur_reader = BytesReader::new(session.reader.read_bytes(length)?);
        //
        // for track in self.tracks.values_mut() {
        //     if let Some(interleaveds) = track.transport.interleaved {
        //         let rtp_identifier = interleaveds[0];
        //         let rtcp_identifier = interleaveds[1];
        //
        //         if channel_identifier == rtp_identifier {
        //             track.on_rtp(&mut cur_reader).await?;
        //         } else if channel_identifier == rtcp_identifier {
        //             track.on_rtcp(&mut cur_reader, session.io.clone()).await;
        //         }
        //     }
        // }
        Ok(())
    }

    async fn handle_options(&mut self, ctx: &mut RtspServerSessionContext, rtsp_request: &RtspRequest) -> Result<Option<RtspResponse>, RtspSessionError> {
        Ok(None)
    }

    async fn handle_describe(&mut self, ctx: &mut RtspServerSessionContext, rtsp_request: &RtspRequest) -> Result<SessionDescription, RtspSessionError> {

        let (sender, mut receiver) = mpsc::unbounded_channel();

        let path = rtsp_request.uri.path.to_string();
        let stream_id = StreamId::Rtsp {
            path
        };

        let request_event = StreamHubEvent::Request{
            stream_id,
            result_sender: sender,
        };

        self.event_producer.send(request_event);

        if let Some(StreamInformation::Sdp { data }) = receiver.recv().await {
            if let Ok(sdp) = SessionDescription::unmarshal(&data) {
                self.sdp = sdp.clone();
                return Ok(sdp);
                //it can new tracks when get the sdp information;
            }
        }

        Err(RtspSessionError::StreamHubEventSendErr)

    }

    async fn handle_announce(&mut self, ctx: &mut RtspServerSessionContext, rtsp_request: &RtspRequest, frame_receiver:FrameDataReceiver) -> Result<Option<RtspResponse>, RtspSessionError> {

        if let Some(request_body) = &rtsp_request.body {
            if let sdp = SessionDescription::unmarshal(request_body)? {
                self.sdp = sdp;
            }
        }

        let (result_sender, result_receiver) = oneshot::channel();

        let path = rtsp_request.uri.path.to_string();

        let publisher_info = StreamPublishInfo {
            stream_id: StreamId::Rtsp {path},
            publish_type: PublishType::Push,
            publisher_id: ctx.session_id.clone(),
        };

        self.publish_info = Some(publisher_info.clone());

        let publish_event = StreamHubEvent::Publish{
            info:publisher_info,
            sdp: self.sdp.clone(),
            receiver:frame_receiver,
            result_sender: result_sender,
            stream_handler: ctx.stream_handler.clone(),

        };

        self.event_producer.send(publish_event);

        let sender = match result_receiver.await? {
            Ok(x) => {
                // self.frame_sender = Some(x);
                info!("rtsp server frame sender settled")
            },
            Err(_) => todo!(),
        };

        Ok(None)
    }

    async fn handle_setup(&mut self, ctx: &mut RtspServerSessionContext, _rtsp_request: &RtspRequest) -> Result<Option<RtspResponse>, RtspSessionError> {
        Ok(None)
    }

    async fn handle_play(&mut self, ctx: &mut RtspServerSessionContext, rtsp_request: &RtspRequest, frame_sender: FrameDataSender) -> Result<Option<RtspResponse>, RtspSessionError> {

        let (event_result_sender, event_result_receiver) = oneshot::channel();
        let path = rtsp_request.uri.path.to_string();

        let subscribe_info = StreamSubscribeInfo {
            stream_id: StreamId::Rtsp {path},
            subscribe_type: SubscribeType::Pull,
            subscriber_id: ctx.session_id.clone(),
        };
        self.subscribe_info = Some(subscribe_info.clone());
        let subscribe_event = StreamHubEvent::Subscribe {
            info: subscribe_info,
            sender: frame_sender,
            result_sender: event_result_sender,
        };

        if self.event_producer.send(subscribe_event).is_err() {
            return Err(RtspSessionError::StreamHubEventSendErr);
        }

        // let mut receiver = event_result_receiver.await?.unwrap();
        // // self.frame_receiver= Some(receiver);

        Ok(None)
    }


    async fn handle_record(&mut self, ctx: &mut RtspServerSessionContext, _rtsp_request: &RtspRequest) -> Result<Option<RtspResponse>, RtspSessionError> {
        Ok(None)
    }

    async fn handle_teardown(&mut self, ctx: &mut RtspServerSessionContext, _rtsp_request: &RtspRequest) -> Result<Option<RtspResponse>, RtspSessionError> {
        Ok(None)
    }
}

pub struct RtspServerHandler {
    hub_event_sender: StreamHubEventSender
}

impl RtspServerHandler
{
    pub fn new(hub_event_sender: StreamHubEventSender) -> Self {
        Self {hub_event_sender}
    }
}

#[async_trait]
impl TcpServerHandler<RtspServerSession> for RtspServerHandler
{
    async fn on_create_session(&mut self, sock: tokio::net::TcpStream, remote: SocketAddr) -> Result<RtspServerSession, SessionError> {
        // info!("Session {} created", session_id);
        let id = Uuid::new(RandomDigitCount::Zero).to_string();
        Ok(RtspServerSession::new(id, sock, remote, Some(Box::new(VcpRtspServerSessionHandler::new(self.hub_event_sender.clone())))))
    }

    async fn on_session_created(&mut self, session_id: String) -> Result<(), SessionError> {
        info!("Session {} created", session_id);
        Ok(())
    }
}


pub struct RtspServer {
    tcp_server: TcpServer<RtspServerSession>,
    // hub_event_sender: StreamHubEventSender,
}


impl RtspServer {
    pub fn new(addr: String, hub_event_sender: StreamHubEventSender) -> Self {
        let server_handler = Box::new(
            RtspServerHandler::new(hub_event_sender)
        );
        let rtsp_server: TcpServer<RtspServerSession> = TcpServer::new(addr, Some(server_handler));

        let res = Self {
            tcp_server: rtsp_server,
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


// fn get_subscriber_info(&mut self) -> SubscriberInfo {
//     let id = if let Some(session_id) = &self.session_id {
//         *session_id
//     } else {
//         Uuid::new(RandomDigitCount::Zero)
//     };

//     SubscriberInfo {
//         id,
//         sub_type: SubscribeType::PlayerRtsp,
//         sub_data_type: streamhub::define::SubDataType::Frame,
//         notify_info: NotifyInfo {
//             request_url: String::from(""),
//             remote_addr: String::from(""),
//         },
//     }
// }

// fn get_publisher_info(&mut self) -> PublisherInfo {
//     let id = if let Some(session_id) = &self.session_id {
//         *session_id
//     } else {
//         Uuid::new(RandomDigitCount::Zero)
//     };

//     PublisherInfo {
//         id,
//         pub_type: PublishType::PushRtsp,
//         pub_data_type: streamhub::define::PubDataType::Frame,
//         notify_info: NotifyInfo {
//             request_url: String::from(""),
//             remote_addr: String::from(""),
//         },
//     }
// }

// pub async fn send_response(&mut self, response: &RtspResponse) -> Result<(), RtspSessionError> {
//     info!("send response:==========================\n{}=============", response);
//
//     self.writer.write(response.marshal().as_bytes())?;
//     self.writer.flush().await?;
//
//     Ok(())
// }
