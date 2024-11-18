// use super::rtsp_codec;

use super::define::rtsp_method_name;
use super::errors::RtspSessionError;
use crate::message::codec;
use crate::message::codec::RtspCodecInfo;
use crate::message::range::RtspRange;
use crate::message::track::RtspTrack;
use crate::message::track::TrackType;
use crate::message::transport::ProtocolType;
use crate::message::transport::RtspTransport;


use async_trait::async_trait;
use byteorder::BigEndian;
use bytes::BytesMut;
use http;
use log::{error, info};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use vcp_media_common::bytesio::bytes_errors::BytesWriteError;
use vcp_media_common::bytesio::bytes_reader::BytesReader;
use vcp_media_common::bytesio::bytes_writer::{AsyncBytesWriter, BytesWriter};
use vcp_media_common::bytesio::bytesio::TNetIO;
use vcp_media_common::bytesio::bytesio::TcpIO;
use vcp_media_common::bytesio::bytesio::UdpIO;
use vcp_media_common::http::{HttpRequest as RtspRequest, HttpResponse};
use vcp_media_common::http::HttpResponse as RtspResponse;
use vcp_media_common::server::{NetworkSession, ServerSessionHandler};
use vcp_media_common::server::TcpSession;
use vcp_media_common::uuid::{RandomDigitCount, Uuid};
use vcp_media_common::Marshal;
use vcp_media_common::Marshal as RtpMarshal;
use vcp_media_common::Unmarshal;
use vcp_media_rtp::RtpPacket;
use vcp_media_sdp::SessionDescription;

pub struct InterleavedBinaryData {
    channel_identifier: u8,
    length: u16,
}

#[async_trait]
pub trait RtspServerSessionHandler : Send + Sync {

    async fn handle_options(&mut self, session: &mut RTSPServerSessionContext, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError>;

    async fn handle_describe(&mut self, session: &mut RTSPServerSessionContext, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> ;

    async fn handle_announce(&mut self, session: &mut RTSPServerSessionContext, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> ;

    async fn handle_setup(&mut self, session: &mut RTSPServerSessionContext, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> ;

    async fn handle_play(&mut self, session: &mut RTSPServerSessionContext, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError>;

    async fn handle_record(&mut self, session: &mut RTSPServerSessionContext, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> ;

    async fn handle_teardown(&mut self, session: &mut RTSPServerSessionContext, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> ;
}

impl InterleavedBinaryData {
    // 10.12 Embedded (Interleaved) Binary Data
    // Stream data such as RTP packets is encapsulated by an ASCII dollar
    // sign (24 hexadecimal), followed by a one-byte channel identifier,
    // followed by the length of the encapsulated binary data as a binary,
    // two-byte integer in network byte order
    fn new(reader: &mut BytesReader) -> Result<Option<Self>, RtspSessionError> {
        let is_dollar_sign = reader.advance_u8()? == 0x24;
        log::debug!("dollar sign: {}", is_dollar_sign);
        if is_dollar_sign {
            reader.read_u8()?;
            let channel_identifier = reader.read_u8()?;
            log::debug!("channel_identifier: {}", channel_identifier);
            let length = reader.read_u16::<BigEndian>()?;
            log::debug!("length: {}", length);
            return Ok(Some(InterleavedBinaryData {
                channel_identifier,
                length,
            }));
        }
        Ok(None)
    }
}

pub struct RTSPServerSessionContext{
    io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>,
    reader: BytesReader,
    writer: BytesWriter,
}


impl RTSPServerSessionContext {

    fn new(io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>) -> Self {
        Self{
            io,
            reader: BytesReader::new(BytesMut::default()),
            writer: BytesWriter::new(),
        }
    }

    pub(crate) fn clone(&self) -> Self {
        Self::new(self.io.clone())
    }

    pub fn get_io(&self) -> Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>{
        self.io.clone()
    }

    pub async fn flush(&mut self) -> Result<(), BytesWriteError> {
        self.io
            .lock()
            .await
            .write(self.writer.bytes.clone().into())
            .await?;
        self.writer.bytes.clear();
        Ok(())
    }


    pub async fn send_response(&mut self, response: &RtspResponse) -> Result<(), RtspSessionError> {
        info!("send response:==========================\n{}=============", response);

        self.writer.write(response.marshal().as_bytes())?;
        self.flush().await?;

        Ok(())
    }
}

pub struct RTSPServerSession {
    id: String,
    remote_addr: SocketAddr,
    context: RTSPServerSessionContext,
    io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>,
    reader: BytesReader,
    writer: AsyncBytesWriter,
    // tracks: HashMap<TrackType, RtspTrack>,
    // sdp: SessionDescription,
    // session_id: Option<Uuid>,
    session_handler: Option<Box<dyn RtspServerSessionHandler>>,


    // stream_handler: Arc<RtspStreamHandler>,
    // event_producer: StreamHubEventSender,

    // auth: Option<Auth>,
}


#[async_trait]
impl NetworkSession for RTSPServerSession {
    fn id(&self) -> String {
        return self.id.clone();
    }

    fn session_type(&self) -> String {
        return "RTSP".to_string();
    }
    //
    // fn set_handler(&mut self, handler: Box<dyn ServerSessionHandler>) {
    //     self.session_handler = Some(handler)
    // }


    async fn run(&mut self) {
        let res = self.handle_session().await;
        match res{
            Ok(_) => info!("{} session {} ended.", self.session_type(), self.id()),
            Err(e) => {
                error!("{} session {} error:{}", self.session_type(), self.id(), e)
            }
        }
    }
}

impl TcpSession for RTSPServerSession {
    fn from_tcp_socket(sock: TcpStream, remote: SocketAddr) -> Self {
        let id = Uuid::new(RandomDigitCount::Zero).to_string();
        Self::new(id, sock, remote, None)
    }

    // async fn notify_created(&mut self) {
    //     if let Some(handler) = self.session_handler.as_mut(){
    //         handler.handle_created().await
    //     }else {
    //         Err(RtspSessionError::NoSessionHandlerError)
    //     }
    // }
}

impl RTSPServerSession {
    pub fn new(
        id: String,
        stream: TcpStream,
        remote: SocketAddr,
        handler: Option<Box<dyn RtspServerSessionHandler>>
        // event_producer: StreamHubEventSender,
        // auth: Option<Auth>,
    ) -> Self {
        // let remote_addr = if let Ok(addr) = stream.peer_addr() {
        //     log::info!("server session: {}", addr.to_string());
        //     Some(addr)
        // } else {
        //     None
        // };

        let net_io: Box<dyn TNetIO + Send + Sync> = Box::new(TcpIO::new(stream));
        let io = Arc::new(Mutex::new(net_io));

        Self {
            id,
            io: io.clone(),
            context: RTSPServerSessionContext::new(io.clone()),
            reader: BytesReader::new(BytesMut::default()),
            writer: AsyncBytesWriter::new(io),

            remote_addr: remote,
            // stream_handler: Arc::new(RtspStreamHandler::new()),
            session_handler: handler,
        }
    }

    pub async fn handle_session(&mut self) -> Result<(), RtspSessionError> {
        loop {
            while self.reader.len() < 4 {
                let data = self.io.lock().await.read().await?;
                self.reader.extend_from_slice(&data[..]);
            }

            if let Ok(data) = InterleavedBinaryData::new(&mut self.reader) {
                match data {
                    Some(a) => {
                        if self.reader.len() < a.length as usize {
                            let data = self.io.lock().await.read().await?;
                            self.reader.extend_from_slice(&data[..]);
                        }
                        self.on_rtp_over_rtsp_message(a.channel_identifier, a.length as usize)
                            .await?;
                    }
                    None => {
                        self.on_rtsp_message().await?;
                    }
                }
            }
        }
    }

    async fn on_rtp_over_rtsp_message(
        &mut self,
        channel_identifier: u8,
        length: usize,
    ) -> Result<(), RtspSessionError> {
        // let mut cur_reader = BytesReader::new(self.reader.read_bytes(length)?);

        // for track in self.tracks.values_mut() {
        //     if let Some(interleaveds) = track.transport.interleaved {
        //         let rtp_identifier = interleaveds[0];
        //         let rtcp_identifier = interleaveds[1];
        //
        //         if channel_identifier == rtp_identifier {
        //             track.on_rtp(&mut cur_reader).await?;
        //         } else if channel_identifier == rtcp_identifier {
        //             track.on_rtcp(&mut cur_reader, self.io.clone()).await;
        //         }
        //     }
        // }
        Ok(())
    }

    //publish stream: OPTIONS->ANNOUNCE->SETUP->RECORD->TEARDOWN
    //subscribe stream: OPTIONS->DESCRIBE->SETUP->PLAY->TEARDOWN
    async fn on_rtsp_message(&mut self) -> Result<(), RtspSessionError> {
        // let data = self.reader.extract_remaining_bytes();

        let data = self.reader.get_remaining_bytes();


        log::debug!("received rtsp session data, length {}", data.len());

        if let Ok(rtsp_request) = RtspRequest::unmarshal(std::str::from_utf8(&data)?) {
            info!("received rtsp request session:{}", rtsp_request);
            self.reader.read_bytes(rtsp_request.origin_length);

            match rtsp_request.method.as_str() {
                rtsp_method_name::OPTIONS => {
                    self.handle_options(&rtsp_request).await?;
                }
                rtsp_method_name::DESCRIBE => {
                    self.handle_describe(&rtsp_request).await?;
                }
                rtsp_method_name::ANNOUNCE => {
                    self.handle_announce(&rtsp_request).await?;
                }
                rtsp_method_name::SETUP => {
                    self.handle_setup(&rtsp_request).await?;
                }
                rtsp_method_name::PLAY => {
                    self.handle_play(&rtsp_request).await?
                }
                rtsp_method_name::RECORD => {
                    self.handle_record(&rtsp_request).await?;
                }
                rtsp_method_name::TEARDOWN => {
                    self.handle_teardown(&rtsp_request).await?;
                }
                rtsp_method_name::PAUSE => {}
                rtsp_method_name::GET_PARAMETER => {}
                rtsp_method_name::SET_PARAMETER => {}
                rtsp_method_name::REDIRECT => {}

                _ => {}
            }
        } else {
            log::debug!("not a valid rtsp request message");
            let data = self.io.lock().await.read().await?;
            self.reader.extend_from_slice(&data[..]);
        }

        Ok(())
    }


    pub async fn send_response(&mut self, response: &RtspResponse) -> Result<(), RtspSessionError> {
        info!("send response:==========================\n{}=============", response);

        self.writer.write(response.marshal().as_bytes())?;
        self.writer.flush().await?;

        Ok(())
    }

    // pub fn get_io(&self) -> Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>{
    //     self.io.clone()
    // }
}

impl RTSPServerSession{
    // async fn on_created(&mut self, session: Arc<Box<RTSPServerSession>>) -> Result<(), RtspSessionError> {
    //     if let Some(handler) = self.session_handler.as_mut(){
    //         handler.on_created(session).await
    //     }else {
    //         Err(RtspSessionError::NoSessionHandlerError)
    //     }
    // }

    async fn handle_options(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        if let Some(handler) = self.session_handler.as_mut(){
            handler.handle_options(&mut self.context.clone(), rtsp_request).await
        }else {
            Err(RtspSessionError::NoSessionHandlerError)
        }


    }

    async fn handle_describe(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        if let Some(handler) = self.session_handler.as_mut(){
            handler.handle_describe(&mut self.context.clone(), rtsp_request).await
        }else {
            Err(RtspSessionError::NoSessionHandlerError)
        }
    }

    async fn handle_announce(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        if let Some(handler) = self.session_handler.as_mut(){
            handler.handle_announce(&mut self.context.clone(), rtsp_request).await
        }else {
            Err(RtspSessionError::NoSessionHandlerError)
        }
    }

    async fn handle_setup(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        if let Some(handler) = self.session_handler.as_mut(){
            handler.handle_setup(&mut self.context.clone(), rtsp_request).await
        }else {
            Err(RtspSessionError::NoSessionHandlerError)
        }
    }

    async fn handle_play(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        if let Some(handler) = self.session_handler.as_mut(){
            handler.handle_play(&mut self.context.clone(), rtsp_request).await
        }else {
            Err(RtspSessionError::NoSessionHandlerError)
        }
    }

    async fn handle_record(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        if let Some(handler) = self.session_handler.as_mut(){
            handler.handle_record(&mut self.context.clone(), rtsp_request).await
        }else {
            Err(RtspSessionError::NoSessionHandlerError)
        }
    }

    async fn handle_teardown(&mut self, rtsp_request: &RtspRequest) -> Result<(), RtspSessionError> {
        if let Some(handler) = self.session_handler.as_mut(){
            handler.handle_teardown(&mut self.context.clone(), rtsp_request).await
        }else {
            Err(RtspSessionError::NoSessionHandlerError)
        }
    }
}

#[derive(Default)]
pub struct RtspStreamHandler {
    sdp: Mutex<SessionDescription>,
}

impl RtspStreamHandler {
    pub fn new() -> Self {
        Self {
            sdp: Mutex::new(SessionDescription::default()),
        }
    }
    pub async fn set_sdp(&self, sdp: SessionDescription) {
        *self.sdp.lock().await = sdp;
    }
}

// #[async_trait]
// impl TStreamHandler for RtspStreamHandler {
//     async fn send_prior_data(
//         &self,
//         data_sender: DataSender,
//         sub_type: SubscribeType,
//     ) -> Result<(), ChannelError> {
//         let sender = match data_sender {
//             DataSender::Frame { sender } => sender,
//             DataSender::Packet { sender: _ } => {
//                 return Err(ChannelError {
//                     value: ChannelError::NotCorrectDataSenderType,
//                 });
//             }
//         };
//         match sub_type {
//             SubscribeType::PlayerRtmp => {
//                 let sdp_info = self.sdp.lock().await;
//                 let mut video_clock_rate: u32 = 0;
//                 let mut audio_clock_rate: u32 = 0;

//                 let mut vcodec: VideoCodecType = VideoCodecType::H264;

//                 for media in &sdp_info.medias {
//                     let mut bytes_writer = BytesWriter::new();
//                     if let Some(fmtp) = &media.fmtp {
//                         match fmtp {
//                             Fmtp::H264(data) => {
//                                 bytes_writer.write(&ANNEXB_NALU_START_CODE)?;
//                                 bytes_writer.write(&data.sps)?;
//                                 bytes_writer.write(&ANNEXB_NALU_START_CODE)?;
//                                 bytes_writer.write(&data.pps)?;

//                                 let frame_data = FrameData::Video {
//                                     timestamp: 0,
//                                     data: bytes_writer.extract_current_bytes(),
//                                 };
//                                 if let Err(err) = sender.send(frame_data) {
//                                     log::error!("send sps/pps error: {}", err);
//                                 }
//                                 video_clock_rate = media.rtpmap.clock_rate;
//                             }
//                             Fmtp::H265(data) => {
//                                 bytes_writer.write(&ANNEXB_NALU_START_CODE)?;
//                                 bytes_writer.write(&data.sps)?;
//                                 bytes_writer.write(&ANNEXB_NALU_START_CODE)?;
//                                 bytes_writer.write(&data.pps)?;
//                                 bytes_writer.write(&ANNEXB_NALU_START_CODE)?;
//                                 bytes_writer.write(&data.vps)?;

//                                 let frame_data = FrameData::Video {
//                                     timestamp: 0,
//                                     data: bytes_writer.extract_current_bytes(),
//                                 };
//                                 if let Err(err) = sender.send(frame_data) {
//                                     log::error!("send sps/pps/vps error: {}", err);
//                                 }

//                                 vcodec = VideoCodecType::H265;
//                             }
//                             Fmtp::Mpeg4(data) => {
//                                 let frame_data = FrameData::Audio {
//                                     timestamp: 0,
//                                     data: data.asc.clone(),
//                                 };

//                                 if let Err(err) = sender.send(frame_data) {
//                                     log::error!("send asc error: {}", err);
//                                 }

//                                 audio_clock_rate = media.rtpmap.clock_rate;
//                             }
//                         }
//                     }
//                 }

//                 if let Err(err) = sender.send(FrameData::MediaInfo {
//                     media_info: MediaInfo {
//                         audio_clock_rate,
//                         video_clock_rate,

//                         vcodec,
//                     },
//                 }) {
//                     log::error!("send media info error: {}", err);
//                 }
//             }
//             SubscribeType::PlayerHls => {}
//             _ => {}
//         }

//         Ok(())
//     }
//     // async fn get_statistic_data(&self) -> Option<StreamStatistics> {
//     //     None
//     // }

//     // async fn send_information(&self, sender: InformationSender) {
//     //     if let Err(err) = sender.send(Information::Sdp {
//     //         data: self.sdp.lock().await.marshal(),
//     //     }) {
//     //         log::error!("send_information of rtsp error: {}", err);
//     //     }
//     // }
// }