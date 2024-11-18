use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc};
use async_trait::async_trait;
use byteorder::BigEndian;
use log::info;
use tokio::sync::Mutex;
use vcp_media_common::bytesio::bytes_writer::AsyncBytesWriter;
use vcp_media_common::bytesio::bytesio::{TNetIO, UdpIO};
use vcp_media_common::{Marshal, Unmarshal};
use vcp_media_common::server::tcp_server::TcpServer;
use vcp_media_rtp::RtpPacket;
use vcp_media_rtsp::message::range::RtspRange;
use vcp_media_rtsp::message::transport::{ProtocolType, RtspTransport};
use vcp_media_rtsp::session::define::rtsp_method_name;
use vcp_media_rtsp::session::errors::RtspSessionError;
use vcp_media_rtsp::session::server_session::{RTSPServerSession, RtspServerSessionHandler};

use vcp_media_common::http::HttpRequest as RtspRequest;
use vcp_media_common::http::HttpResponse as RtspResponse;
use vcp_media_common::server::{NetworkServer, NetworkSession, ServerHandler, SessionError};
use vcp_media_common::uuid::{RandomDigitCount, Uuid};
use vcp_media_rtsp::message::codec;
use vcp_media_rtsp::message::codec::RtspCodecInfo;
use vcp_media_rtsp::message::track::{RtspTrack, TrackType};
use vcp_media_sdp::SessionDescription;


pub struct RtspServerHandler;
#[async_trait]
impl ServerHandler for RtspServerHandler {
    async fn on_session_created(&mut self, session_id: String) -> Result<(), SessionError> {
        info!("Session {} created", session_id);
        Ok(())
    }
}


pub struct RtspServer {
    tcp_server: TcpServer<RTSPServerSession>,
}


impl RtspServer {
    pub fn new(addr:String) -> Self{
        let server_handler = Box::new(
            RtspServerHandler
        );
        let mut rtsp_server: TcpServer<RTSPServerSession> = TcpServer::new(addr, Some(server_handler));

        let res = Self{
            tcp_server: rtsp_server,
        };
        res

    }

    pub fn session_type(&self) -> String{
        self.tcp_server.session_type()
    }

    pub async fn start(&mut self) -> Result<(), SessionError> {
        self.tcp_server.start().await
    }
}



pub struct VcpRtspServerSessionHandler {
    session: Arc<Mutex<RTSPServerSession>>,
    tracks: HashMap<TrackType, RtspTrack>,
    sdp: SessionDescription,
    session_id: Option<Uuid>,
}
impl VcpRtspServerSessionHandler {

    pub fn new(session: Arc<Mutex<RTSPServerSession>> ) -> Self {
        Self{
            session,
            tracks: HashMap::new(),
            sdp: SessionDescription::default(),
            session_id: None,
        }

    }
    fn new_tracks(&mut self) -> Result<(), RtspSessionError> {
        for media in &self.sdp.medias {
            let media_control = media.get_control();

            if let Some(rtpmap) = &media.rtpmap {
                let media_name = &media.media_type;
                log::info!("media_name: {}", media_name);
                match media_name.as_str() {
                    "audio" => {
                        let codec_id = codec::RTSP_CODEC_NAME_2_ID
                            .get(&rtpmap.encoding_name.to_lowercase().as_str())
                            .unwrap()
                            .clone();
                        let codec_info = RtspCodecInfo {
                            codec_id,
                            payload_type: rtpmap.payload_type as u8,
                            sample_rate: rtpmap.clock_rate,
                            channel_count: rtpmap.encoding_param.parse().unwrap(),
                        };

                        log::info!("audio codec info: {:?}", codec_info);

                        let track = RtspTrack::new(TrackType::Audio, codec_info, media_control);
                        self.tracks.insert(TrackType::Audio, track);
                    }
                    "video" => {
                        let codec_id = codec::RTSP_CODEC_NAME_2_ID
                            .get(&rtpmap.encoding_name.to_lowercase().as_str())
                            .unwrap()
                            .clone();
                        let codec_info = RtspCodecInfo {
                            codec_id,
                            payload_type: rtpmap.payload_type as u8,
                            sample_rate: rtpmap.clock_rate,
                            ..Default::default()
                        };

                        log::info!("video codec info: {:?}", codec_info);

                        let track = RtspTrack::new(TrackType::Video, codec_info, media_control);
                        self.tracks.insert(TrackType::Video, track);
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn gen_response(status_code: http::StatusCode, rtsp_request: &RtspRequest) -> RtspResponse {
        let reason_phrase = if let Some(reason) = status_code.canonical_reason() {
            reason.to_string()
        } else {
            "".to_string()
        };

        let mut response = RtspResponse {
            version: "RTSP/1.0".to_string(),
            status_code: status_code.as_u16(),
            reason_phrase,
            ..Default::default()
        };

        if let Some(cseq) = rtsp_request.headers.get("CSeq") {
            response
                .headers
                .insert("CSeq".to_string(), cseq.to_string());
        }

        response
    }

    // pub fn get_session_io(&mut self) -> Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>{
    //     self.session.lock().get_io()
    // }

    pub async fn send_response(&mut self, response: &RtspResponse) -> Result<(), RtspSessionError> {
        self.session.lock().await.send_response(response).await
    }

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
    async fn handle_options(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        let status_code = http::StatusCode::OK;
        let mut response = Self::gen_response(status_code, rtsp_request);
        let public_str = rtsp_method_name::ARRAY.join(",");
        response.headers.insert("Public".to_string(), public_str);
        self.send_response(&response).await?;

        Ok(())
    }

    async fn handle_describe(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        let status_code = http::StatusCode::OK;

        // The sender is used for sending sdp information from the server session to client session
        // receiver is used to receive the sdp information
        // let (sender, mut receiver) = mpsc::unbounded_channel<String>();

        // let request_event = StreamHubEvent::Request {
        //     identifier: StreamIdentifier::Rtsp {
        //         stream_path: rtsp_request.uri.path.clone(),
        //     },
        //     sender,
        // };

        // if self.event_producer.send(request_event).is_err() {
        //     return Err(SessionError {
        //         value: SessionError::StreamHubEventSendErr,
        //     });
        // }

        // if let Some(Information::Sdp { data }) = receiver.recv().await {
        //     if let Some(sdp) = Sdp::unmarshal(&data) {
        //         self.sdp = sdp;
        //         //it can new tracks when get the sdp information;
        //         self.new_tracks()?;
        //     }
        // }

        let mut response = Self::gen_response(status_code, rtsp_request);
        let sdp = self.sdp.marshal();
        log::debug!("sdp: {}", sdp);
        response.body = Some(sdp);
        response
            .headers
            .insert("Content-Type".to_string(), "application/sdp".to_string());
        self.send_response(&response).await?;

        Ok(())
    }

    async fn handle_announce(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        // if let Some(auth) = &self.auth {
        //     let stream_name = rtsp_request.uri.path.clone();
        //     auth.authenticate(&stream_name, &rtsp_request.uri.query, false)?;
        // }

        if let Some(request_body) = &rtsp_request.body {
            if let sdp = SessionDescription::unmarshal(request_body)? {
                self.sdp = sdp.clone();
                // self.stream_handler.set_sdp(sdp).await;
            }
        }

        //new tracks for publish session
        self.new_tracks()?;

        // let (event_result_sender, event_result_receiver) = oneshot::channel();

        // let publish_event = StreamHubEvent::Publish {
        //     identifier: StreamIdentifier::Rtsp {
        //         stream_path: rtsp_request.uri.path.clone(),
        //     },
        //     result_sender: event_result_sender,
        //     info: self.get_publisher_info(),
        //     stream_handler: self.stream_handler.clone(),
        // };

        // if self.event_producer.send(publish_event).is_err() {
        //     return Err(SessionError {
        //         value: SessionError::StreamHubEventSendErr,
        //     });
        // }

        // let sender = event_result_receiver.await??.0.unwrap();

        // for track in self.tracks.values_mut() {
        //     let sender_out = sender.clone();
        //     let mut rtp_channel_guard = track.rtp_channel.lock().await;

        //     // rtp_channel_guard.on_frame_handler(Box::new(
        //     //     move |msg: FrameData| -> Result<(), UnPackerError> {
        //     //         if let Err(err) = sender_out.send(msg) {
        //     //             log::error!("send frame error: {}", err);
        //     //         }
        //     //         Ok(())
        //     //     },
        //     // ));

        //     let rtcp_channel = Arc::clone(&track.rtcp_channel);
        //     rtp_channel_guard.on_packet_for_rtcp_handler(Box::new(move |packet: RtpPacket| {
        //         let rtcp_channel_in = Arc::clone(&rtcp_channel);
        //         Box::pin(async move {
        //             rtcp_channel_in.lock().await.on_packet(packet);
        //         })
        //     }));
        // }

        let status_code = http::StatusCode::OK;
        let response = Self::gen_response(status_code, rtsp_request);
        self.send_response(&response).await?;

        Ok(())
    }

    async fn handle_setup(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        let status_code = http::StatusCode::OK;
        let mut response = Self::gen_response(status_code, rtsp_request);

        for track in self.tracks.values_mut() {
            if !rtsp_request.uri.marshal().contains(&track.media_control) {
                continue;
            }

            if let Some(transport_data) = rtsp_request.get_header(&"Transport".to_string()) {
                if self.session_id.is_none() {
                    self.session_id = Some(Uuid::new(RandomDigitCount::Zero));
                }

                let transport = RtspTransport::unmarshal(transport_data);

                if let Ok(mut trans) = transport {
                    let mut rtp_server_port: Option<u16> = None;
                    let mut rtcp_server_port: Option<u16> = None;

                    match trans.protocol_type {
                        ProtocolType::TCP => {
                            // track.create_packer(self.io.clone()).await;

                            let io = self.session.lock().await.get_io();

                            track.create_packer(io).await;

                        }
                        ProtocolType::UDP => {
                            let (rtp_port, rtcp_port) =
                                if let Some(client_ports) = trans.client_port {
                                    (client_ports[0], client_ports[1])
                                } else {
                                    log::error!("should not be here!!");
                                    (0, 0)
                                };

                            let address = rtsp_request.uri.host.clone();
                            if let Some(rtp_io) = UdpIO::new(address.clone(), rtp_port, 0).await {
                                rtp_server_port = rtp_io.get_local_port();

                                let box_udp_io: Box<dyn TNetIO + Send + Sync> = Box::new(rtp_io);
                                //if mode is empty then it is a player session.
                                if trans.transport_mod.is_none() {
                                    track.create_packer(Arc::new(Mutex::new(box_udp_io))).await;
                                } else {
                                    track.rtp_receive_loop(box_udp_io).await;
                                }
                            }

                            if let Some(rtcp_io) =
                                UdpIO::new(address.clone(), rtcp_port, rtp_server_port.unwrap() + 1)
                                    .await
                            {
                                rtcp_server_port = rtcp_io.get_local_port();
                                let box_rtcp_io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>> =
                                    Arc::new(Mutex::new(Box::new(rtcp_io)));
                                track.rtcp_receive_loop(box_rtcp_io).await;
                            }
                        }
                    }

                    //tell client the udp ports of server side
                    let mut server_ports: [u16; 2] = [0, 0];
                    if let Some(rtp_port) = rtp_server_port {
                        server_ports[0] = rtp_port;
                    }
                    if let Some(rtcp_server_port) = rtcp_server_port {
                        server_ports[1] = rtcp_server_port;
                        trans.server_port = Some(server_ports);
                    }

                    let new_transport_data = trans.marshal();
                    response
                        .headers
                        .insert("Transport".to_string(), new_transport_data);
                    response
                        .headers
                        .insert("Session".to_string(), self.session_id.unwrap().to_string());

                    track.set_transport(trans).await;
                }
            }
            break;
        }

        self.send_response(&response).await?;

        Ok(())
    }

    async fn handle_play(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        // if let Some(auth) = &self.auth {
        //     let stream_name = rtsp_request.uri.path.clone();
        //     auth.authenticate(&stream_name, &rtsp_request.uri.query, true)?;
        // }

        for track in self.tracks.values_mut() {
            let protocol_type = track.transport.protocol_type.clone();

            match protocol_type {
                ProtocolType::TCP => {
                    let channel_identifer = if let Some(interleaveds) = track.transport.interleaved
                    {
                        interleaveds[0]
                    } else {
                        log::error!("handle_play:should not be here!!!");
                        0
                    };

                    track.rtp_channel.lock().await.on_packet_handler(Box::new(
                        move |io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>, packet: RtpPacket| {
                            Box::pin(async move {
                                let msg = packet.marshal()?;
                                let mut bytes_writer = AsyncBytesWriter::new(io);
                                bytes_writer.write_u8(0x24)?;
                                bytes_writer.write_u8(channel_identifer)?;
                                bytes_writer.write_u16::<BigEndian>(msg.len() as u16)?;
                                bytes_writer.write(&msg)?;
                                bytes_writer.flush().await?;
                                Ok(())
                            })
                        },
                    ));
                }
                ProtocolType::UDP => {
                    track.rtp_channel.lock().await.on_packet_handler(Box::new(
                        move |io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>, packet: RtpPacket| {
                            Box::pin(async move {
                                let mut bytes_writer = AsyncBytesWriter::new(io);

                                let msg = packet.marshal()?;
                                bytes_writer.write(&msg)?;
                                bytes_writer.flush().await?;
                                Ok(())
                            })
                        },
                    ));
                }
            }
        }

        let status_code = http::StatusCode::OK;
        let response = Self::gen_response(status_code, rtsp_request);

        self.send_response(&response).await?;

        Ok(())

        // let (event_result_sender, event_result_receiver) = oneshot::channel();

        // let publish_event = StreamHubEvent::Subscribe {
        //     identifier: StreamIdentifier::Rtsp {
        //         stream_path: rtsp_request.uri.path.clone(),
        //     },
        //     info: self.get_subscriber_info(),
        //     result_sender: event_result_sender,
        // };

        // if self.event_producer.send(publish_event).is_err() {
        //     return Err(SessionError {
        //         value: SessionError::StreamHubEventSendErr,
        //     });
        // }

        // let mut receiver = event_result_receiver.await?.frame_receiver.unwrap();

        // let mut retry_times = 0;
        // loop {
        //     if let Some(frame_data) = receiver.recv().await {
        //         match frame_data {
        //     FrameData::Audio {
        //         timestamp,
        //         mut data,
        //     } => {
        //         if let Some(audio_track) = self.tracks.get_mut(&TrackType::Audio) {
        //             audio_track
        //                 .rtp_channel
        //                 .lock()
        //                 .await
        //                 .on_frame(&mut data, timestamp)
        //                 .await?;
        //         }
        //     }
        //     FrameData::Video {
        //         timestamp,
        //         mut data,
        //     } => {
        //         if let Some(video_track) = self.tracks.get_mut(&TrackType::Video) {
        //             video_track
        //                 .rtp_channel
        //                 .lock()
        //                 .await
        //                 .on_frame(&mut data, timestamp)
        //                 .await?;
        //         }
        //     }
        //             _ => {}
        //         }
        //     } else {
        //         retry_times += 1;
        //         log::info!(
        //             "send_channel_data: no data receives ,retry {} times!",
        //             retry_times
        //         );

        //         if retry_times > 10 {
        //             return Err(SessionError {
        //                 value: SessionError::CannotReceiveFrameData,
        //             });
        //         }
        //     }
        // }
    }


    async fn handle_record(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        if let Some(range_str) = rtsp_request.headers.get(&String::from("Range")) {
            if let Ok(range) = RtspRange::unmarshal(range_str) {
                let status_code = http::StatusCode::OK;
                let mut response = Self::gen_response(status_code, rtsp_request);
                response
                    .headers
                    .insert(String::from("Range"), range.marshal());
                response
                    .headers
                    .insert("Session".to_string(), self.session_id.unwrap().to_string());

                self.send_response(&response).await?;

                return Ok(());
            } else {
                return Err(RtspSessionError::RecordRangeError);
            }
        }

        Err(RtspSessionError::RecordRangeError)
    }

    async fn handle_teardown(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        let _stream_path = &rtsp_request.uri.path;
        // let unpublish_event = StreamHubEvent::UnPublish {
        //     identifier: StreamIdentifier::Rtsp {
        //         stream_path: stream_path.clone(),
        //     },
        //     info: self.get_publisher_info(),
        // };

        // let rv = self.event_producer.send(unpublish_event);
        // match rv {
        //     Err(_) => {
        //         log::error!("unpublish_to_channels error.stream_name: {}", stream_path);
        //         Err(SessionError {
        //             value: SessionError::StreamHubEventSendErr,
        //         })
        //     }
        //     Ok(()) => {
        //         log::info!(
        //             "unpublish_to_channels successfully.stream name: {}",
        //             stream_path
        //         );
        //         Ok(())
        //     }
        // }

        Ok(())
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
