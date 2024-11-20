use async_trait::async_trait;
use byteorder::BigEndian;
use log::{debug, info};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use vcp_media_common::bytesio::bytes_reader::BytesReader;
use vcp_media_common::bytesio::bytes_writer::AsyncBytesWriter;
use vcp_media_common::bytesio::bytesio::{TNetIO, UdpIO};
use vcp_media_common::server::tcp_server::TcpServer;
use vcp_media_common::{Marshal, Unmarshal};
use vcp_media_rtp::RtpPacket;
use vcp_media_rtsp::message::range::RtspRange;
use vcp_media_rtsp::message::transport::{ProtocolType, RtspTransport};
use vcp_media_rtsp::session::define::rtsp_method_name;
use vcp_media_rtsp::session::errors::RtspSessionError;
use vcp_media_rtsp::session::server_session::{RTSPServerSession, RTSPServerSessionContext, RtspServerSessionHandler};

use crate::manager::message;
use crate::manager::message::{StreamHubEvent, StreamHubEventSender, StreamPublishInfo, StreamSubscribeInfo};
use vcp_media_common::http::HttpRequest as RtspRequest;
use vcp_media_common::http::HttpResponse as RtspResponse;
use vcp_media_common::media::{FrameData, FrameDataReceiver, FrameDataSender, StreamInformation};
use vcp_media_common::media::StreamInformation::Sdp;
use vcp_media_common::server::{NetworkServer, NetworkSession, SessionError, TcpServerHandler, TcpSession};
use vcp_media_common::uuid::{RandomDigitCount, Uuid};
use vcp_media_rtsp::message::codec;
use vcp_media_rtsp::message::codec::RtspCodecInfo;
use vcp_media_rtsp::message::track::{RtspTrack, TrackType};
use vcp_media_sdp::SessionDescription;

pub struct VcpRtspServerSessionHandler {
    // session: Option<Arc<Mutex<RTSPServerSession>>>,
    // tracks: HashMap<TrackType, RtspTrack>,
    sdp: SessionDescription,
    // session_id: Option<Uuid>,
    event_producer: StreamHubEventSender,
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
impl RtspServerSessionHandler for VcpRtspServerSessionHandler {
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

    async fn handle_rtp_over_rtsp_message(&mut self, session: &mut RTSPServerSessionContext, channel_identifier: u8, length: usize) -> Result<(), RtspSessionError> {
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

    async fn handle_options(&mut self, rtsp_request: &RtspRequest) -> Result<Option<RtspResponse>, RtspSessionError> {
        Ok(None)
    }

    async fn handle_describe(&mut self, rtsp_request: &RtspRequest) -> Result<SessionDescription, RtspSessionError> {

        let (sender, mut receiver) = mpsc::unbounded_channel();

        let request_event = StreamHubEvent::Request{
            stream_id: "1".to_string(),
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

    async fn handle_announce(&mut self, rtsp_request: &RtspRequest, frame_receiver:FrameDataReceiver) -> Result<Option<RtspResponse>, RtspSessionError> {

        if let Some(request_body) = &rtsp_request.body {
            if let sdp = SessionDescription::unmarshal(request_body)? {
                self.sdp = sdp;
            }
        }

        let (result_sender, result_receiver) = oneshot::channel();

        let publish_event = StreamHubEvent::Publish{
            info:StreamPublishInfo {
                stream_id: "1".to_string(),
                stream_type: "RTSP".to_string(),
                url: "rtsp://1111.1.1.1.".to_string(),
            },
            sdp: self.sdp.clone(),
            receiver:frame_receiver,
            result_sender: result_sender,
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

    async fn handle_setup(&mut self, _rtsp_request: &RtspRequest) -> Result<Option<RtspResponse>, RtspSessionError> {
        Ok(None)
    }

    async fn handle_play(&mut self, _rtsp_request: &RtspRequest, frame_sender: FrameDataSender) -> Result<Option<RtspResponse>, RtspSessionError> {

        let (event_result_sender, event_result_receiver) = oneshot::channel();

        let publish_event = StreamHubEvent::Subscribe {
            info:StreamSubscribeInfo {
                stream_id: "1".to_string(),
                stream_type: "RTSP".to_string(),
                url: "rtsp://1111.1.1.1.".to_string(),
            },
            sender: frame_sender,
            result_sender: event_result_sender,
        };

        if self.event_producer.send(publish_event).is_err() {
            return Err(RtspSessionError::StreamHubEventSendErr);
        }

        // let mut receiver = event_result_receiver.await?.unwrap();
        // // self.frame_receiver= Some(receiver);

        Ok(None)
    }


    async fn handle_record(&mut self, _rtsp_request: &RtspRequest) -> Result<Option<RtspResponse>, RtspSessionError> {
        Ok(None)
    }

    async fn handle_teardown(&mut self, _rtsp_request: &RtspRequest) -> Result<Option<RtspResponse>, RtspSessionError> {
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
impl TcpServerHandler<RTSPServerSession> for RtspServerHandler
{
    async fn on_create_session(&mut self, sock: tokio::net::TcpStream, remote: SocketAddr) -> Result<RTSPServerSession, SessionError> {
        // info!("Session {} created", session_id);
        let id = Uuid::new(RandomDigitCount::Zero).to_string();
        Ok(RTSPServerSession::new(id, sock, remote, Some(Box::new(VcpRtspServerSessionHandler::new(self.hub_event_sender.clone())))))
    }

    async fn on_session_created(&mut self, session_id: String) -> Result<(), SessionError> {
        info!("Session {} created", session_id);
        Ok(())
    }
}


pub struct RtspServer {
    tcp_server: TcpServer<RTSPServerSession>,
    // hub_event_sender: StreamHubEventSender,
}


impl RtspServer {
    pub fn new(addr: String, hub_event_sender: StreamHubEventSender) -> Self {
        let server_handler = Box::new(
            RtspServerHandler::new(hub_event_sender)
        );
        let mut rtsp_server: TcpServer<RTSPServerSession> = TcpServer::new(addr, Some(server_handler));

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
