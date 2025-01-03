use crate::message::chunk::packetizer::ChunkPacketizer;
use crate::message::chunk::unpacketizer::{ChunkUnpacketizer, UnpackResult};
use crate::message::handshake;
use crate::message::handshake::handshake_server::HandshakeServer;
use crate::message::messages::define::{msg_type_id, RtmpMessageData};
use crate::message::messages::parser::MessageParser;
use crate::message::netstream::writer::NetStreamWriter;
use crate::message::protocol_control_messages::writer::ProtocolControlMessagesWriter;
use crate::session::errors::SessionError;

// use vcp_media_common::bytesio::bytes_reader::BytesReader;
use vcp_media_common::bytesio::bytes_writer::AsyncBytesWriter;
use vcp_media_common::bytesio::bytesio::{TNetIO, TcpIO};
use vcp_media_common::server::{NetworkSession, ServerSessionType, TcpSession};
use vcp_media_common::uuid::{RandomDigitCount, Uuid};
use vcp_media_flv::amf0::Amf0ValueType;
use crate::config;
use crate::message::chunk::define::{chunk_type, csid_type, CHUNK_SIZE};
use crate::message::chunk::errors::UnpackError;
use crate::message::chunk::ChunkInfo;
use crate::message::handshake::define::ServerHandshakeState;
use crate::message::netconnection::writer::{ConnectProperties, NetConnection};
use crate::message::user_control_messages::writer::EventMessagesWriter;
use crate::session::define;
use crate::utils::RtmpUrlParser;
use async_trait::async_trait;
use bytes::BytesMut;
use indexmap::IndexMap;
use log::{error, info};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use vcp_media_common::media::{FrameData, FrameDataReceiver, FrameDataSender};
use crate::session::cache::Cache;
use crate::session::cache::errors::CacheError;

enum ServerSessionState {
    Handshake,
    ReadChunk,
    // OnConnect,
    // OnCreateStream,
    //Publish,
    DeleteStream,
    Play,
}

#[async_trait]
pub trait RtmpServerSessionHandler : Send + Sync{
    // async fn handle_session_error(&mut self, ctx: &mut RtmpServerSessionContext);

    async fn handle_publish(&mut self, ctx: &mut RtmpServerSessionContext, frame_receiver: FrameDataReceiver);

    async fn handle_play(&mut self, ctx: &mut RtmpServerSessionContext, frame_sender: FrameDataSender);

    async fn handle_session_end(&mut self, ctx: &mut RtmpServerSessionContext);
    async fn save_video_data(&self, chunk_body: &BytesMut, timestamp: u32, ) -> Result<(), CacheError>;
    async fn save_audio_data(&self, chunk_body: &BytesMut, timestamp: u32, ) -> Result<(), CacheError>;
    async fn save_metadata(&self, chunk_body: &BytesMut, timestamp: u32) -> Result<(), CacheError>;
}



pub struct RtmpServerSessionContext{
    pub session_id:String,
    pub session_type: ServerSessionType,
    pub request_url: String,
}

impl RtmpServerSessionContext {
    pub fn new(session_id:String) -> Self {
        Self{
            session_id,
            session_type: ServerSessionType::Unknown,
            request_url: "".to_string(),
        }
    }
}

pub struct RtmpServerSession {
    id: String,
    remote_addr: SocketAddr,
     context: RtmpServerSessionContext,
    io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>,

    handshaker: HandshakeServer,
    unpacketizer: ChunkUnpacketizer,
    packetizer: ChunkPacketizer,
    state: ServerSessionState,
    app_name: String,
    stream_name: String,
    connect_properties: ConnectProperties,
    query: Option<String>,
    bytesio_data: BytesMut,
    has_remain_data: bool,
    gop_num: usize,

    data_sender: FrameDataSender,
    data_receiver: FrameDataReceiver,

    handler: Option<Box<dyn RtmpServerSessionHandler>>,
}

#[async_trait]
impl NetworkSession for RtmpServerSession {
    fn id(&self) -> String {
        return self.id.clone();
    }

    fn session_type() -> String {
        return "RTMP".to_string();
    }

    async fn run(&mut self) {
        let res = self.handle_session().await;
        match res {
            Ok(_) => info!("{} session {} ended.", Self::session_type(), self.id()),
            Err(e) => {
                error!("{} session {} error:{}", Self::session_type(), self.id(), e);
                self.notify_session_error().await;
            }
        }
    }

    async fn close(&mut self) {
        info!("close session {}", self.id());
        self.notify_session_end().await;
    }
}

#[async_trait]
impl TcpSession for RtmpServerSession {
    fn from_tcp_socket(sock: TcpStream, remote: SocketAddr) -> Self {
        let id = Uuid::new(RandomDigitCount::Zero).to_string();
        Self::new(id, sock, remote, None)
    }
}

impl RtmpServerSession {
    pub fn new(
        id: String,
        stream: TcpStream,
        remote: SocketAddr,
        handler: Option<Box<dyn RtmpServerSessionHandler>>,
    ) -> Self {
        let net_io: Box<dyn TNetIO + Send + Sync> = Box::new(TcpIO::new(stream));
        let io = Arc::new(Mutex::new(net_io));

        let (init_producer, init_consumer) = mpsc::unbounded_channel();
        Self {
            id: id.clone(),
            remote_addr: remote,
            context: RtmpServerSessionContext::new(id),
            io: io.clone(),
            handshaker: HandshakeServer::new(Arc::clone(&io)),
            unpacketizer: ChunkUnpacketizer::new(),
            packetizer: ChunkPacketizer::new(Arc::clone(&io)),
            state: ServerSessionState::Handshake,
            app_name: String::from(""),
            stream_name: String::from(""),
            connect_properties: ConnectProperties::default(),
            query: None,
            bytesio_data: BytesMut::new(),
            has_remain_data: false,
            gop_num:5,
            data_sender: init_producer,
            data_receiver: init_consumer,
            handler,
        }
    }

    async fn notify_session_error(&mut self) {
        if let Some(handler) = self.handler.as_mut(){
            handler.handle_session_end(&mut self.context).await;
        }
    }

    async fn notify_session_end(&mut self) {
        if let Some(handler) = self.handler.as_mut(){
            handler.handle_session_end(&mut self.context).await;
        }
    }


    async fn notify_publish(&mut self) {
        self.context.session_type = ServerSessionType::Push;
        if let Some(handler) = self.handler.as_mut(){
            let (tx, rx) = mpsc::unbounded_channel();
            self.data_sender = tx;
            handler.handle_publish(&mut self.context, rx).await;
        }
    }

    async fn notify_play(&mut self) {
        self.context.session_type = ServerSessionType::Pull;
        if let Some(handler) = self.handler.as_mut(){
            let (tx, rx) = mpsc::unbounded_channel();
            self.data_receiver = rx;
            handler.handle_play(&mut self.context, tx).await;
        }
    }
    async fn handle_session(&mut self) -> Result<(), SessionError> {
        loop {
            match self.state {
                ServerSessionState::Handshake => {
                    self.handshake().await?;
                }
                ServerSessionState::ReadChunk => {
                    self.read_parse_chunks().await?;
                }
                ServerSessionState::Play => {
                    self.play().await?;
                }
                ServerSessionState::DeleteStream => {
                    return Ok(());
                }
            }
        }
    }

    async fn handshake(&mut self) -> Result<(), SessionError> {
        let mut bytes_len = 0;

        while bytes_len < handshake::define::RTMP_HANDSHAKE_SIZE {
            self.bytesio_data = self.io.lock().await.read().await?;
            bytes_len += self.bytesio_data.len();
            self.handshaker.extend_data(&self.bytesio_data[..]);
        }

        self.handshaker.handshake().await?;

        if let ServerHandshakeState::Finish = self.handshaker.state() {
            self.state = ServerSessionState::ReadChunk;
            let left_bytes = self.handshaker.get_remaining_bytes();
            if !left_bytes.is_empty() {
                self.unpacketizer.extend_data(&left_bytes[..]);
                self.has_remain_data = true;
            }
            info!("[ S->C ] [send_set_chunk_size] ");
            self.send_set_chunk_size().await?;
            return Ok(());
        }

        Ok(())
    }

    async fn read_parse_chunks(&mut self) -> Result<(), SessionError> {
        if !self.has_remain_data {
            match self
                .io
                .lock()
                .await
                .read_timeout(Duration::from_secs(2))
                .await
            {
                Ok(data) => {
                    self.bytesio_data = data;
                }
                Err(err) => {
                    return Err(SessionError::from(err));
                }
            }

            self.unpacketizer.extend_data(&self.bytesio_data[..]);
        }

        self.has_remain_data = false;

        loop {
            match self.unpacketizer.read_chunks() {
                Ok(rv) => {
                    if let UnpackResult::Chunks(chunks) = rv {
                        for chunk_info in chunks {
                            let timestamp = chunk_info.message_header.timestamp;
                            let msg_stream_id = chunk_info.message_header.msg_streamd_id;

                            if let Some(mut msg) = MessageParser::new(chunk_info).parse()? {
                                self.process_messages(&mut msg, &msg_stream_id, &timestamp)
                                    .await?;
                            }
                        }
                    }
                }
                Err(err) => {
                    if let UnpackError::CannotParse = err {
                        // self.common
                        //     .unpublish_to_stream_hub(self.app_name.clone(), self.stream_name.clone())
                        //     .await?;
                        return Err(err)?;
                    }
                    break;
                }
            }
        }
        Ok(())
    }

    async fn play(&mut self) -> Result<(), SessionError> {
        match self.send_channel_data().await {
            Ok(_) => {}
            Err(err) => {
                // self.common
                //     .unsubscribe_from_stream_hub(self.app_name.clone(), self.stream_name.clone())
                //     .await?;
                return Err(err);
            }
        }

        Ok(())
    }

    pub async fn send_set_chunk_size(&mut self) -> Result<(), SessionError> {
        let mut controlmessage =
            ProtocolControlMessagesWriter::new(AsyncBytesWriter::new(self.io.clone()));
        controlmessage.write_set_chunk_size(CHUNK_SIZE).await?;

        Ok(())
    }



    pub async fn send_channel_data(&mut self) -> Result<(), SessionError> {
        let mut retry_times = 0;
        loop {
            if let Some(data) = self.data_receiver.recv().await {
                match data {
                    FrameData::Audio { timestamp, data } => {
                        let data_size = data.len();
                        self.send_audio(data, timestamp).await?;

                        // if let Some(sender) = &self.statistic_data_sender {
                        //     let statistic_audio_data = StatisticData::Audio {
                        //         uuid: Some(self.session_id),
                        //         aac_packet_type: 1,
                        //         data_size,
                        //         duration: 0,
                        //     };
                        //     if let Err(err) = sender.send(statistic_audio_data) {
                        //         log::error!("send statistic_data err: {}", err);
                        //     }
                        // }
                    }
                    FrameData::Video { timestamp, data } => {
                        let data_size = data.len();
                        self.send_video(data, timestamp).await?;

                        // if let Some(sender) = &self.statistic_data_sender {
                        //     let statistic_video_data = StatisticData::Video {
                        //         uuid: Some(self.session_id),
                        //         frame_count: 1,
                        //         data_size,
                        //         is_key_frame: None,
                        //         duration: 0,
                        //     };
                        //     if let Err(err) = sender.send(statistic_video_data) {
                        //         log::error!("send statistic_data err: {}", err);
                        //     }
                        // }
                    }
                    FrameData::MetaData { timestamp, data } => {
                        self.send_metadata(data, timestamp).await?;
                    }
                    _ => {}
                }
            } else {
                retry_times += 1;
                log::debug!(
                    "send_channel_data: no data receives ,retry {} times!",
                    retry_times
                );

                if retry_times > 10 {
                    return Err(SessionError::NoMediaDataReceived);
                }
            }
        }
    }

    pub async fn send_audio(&mut self, data: BytesMut, timestamp: u32) -> Result<(), SessionError> {
        let mut chunk_info = ChunkInfo::new(
            csid_type::AUDIO,
            chunk_type::TYPE_0,
            timestamp,
            data.len() as u32,
            msg_type_id::AUDIO,
            0,
            data,
        );

        // if let Some(packetizer) = &mut self.packetizer {
            self.packetizer.write_chunk(&mut chunk_info).await?;
        // }

        Ok(())
    }

    pub async fn send_video(&mut self, data: BytesMut, timestamp: u32) -> Result<(), SessionError> {
        let mut chunk_info = ChunkInfo::new(
            csid_type::VIDEO,
            chunk_type::TYPE_0,
            timestamp,
            data.len() as u32,
            msg_type_id::VIDEO,
            0,
            data,
        );

        // if let Some(packetizer) = &mut self.packetizer {
            self.packetizer.write_chunk(&mut chunk_info).await?;
        // }

        Ok(())
    }

    pub async fn send_metadata(
        &mut self,
        data: BytesMut,
        timestamp: u32,
    ) -> Result<(), SessionError> {
        let mut chunk_info = ChunkInfo::new(
            csid_type::DATA_AMF0_AMF3,
            chunk_type::TYPE_0,
            timestamp,
            data.len() as u32,
            msg_type_id::DATA_AMF0,
            0,
            data,
        );

        // if let Some(packetizer) = &mut self.packetizer {
            self.packetizer.write_chunk(&mut chunk_info).await?;
        // }

        Ok(())
    }

    pub async fn process_messages(
        &mut self,
        rtmp_msg: &mut RtmpMessageData,
        msg_stream_id: &u32,
        timestamp: &u32,
    ) -> Result<(), SessionError> {
        match rtmp_msg {
            RtmpMessageData::Amf0Command {
                command_name,
                transaction_id,
                command_object,
                others,
            } => {
                self.on_amf0_command_message(
                    msg_stream_id,
                    command_name,
                    transaction_id,
                    command_object,
                    others,
                )
                    .await?
            }
            RtmpMessageData::SetChunkSize { chunk_size } => {
                self.on_set_chunk_size(*chunk_size as usize)?;
            }
            RtmpMessageData::AudioData { data } => {
                self.on_audio_data(data, timestamp).await?;
            }
            RtmpMessageData::VideoData { data } => {
                self.on_video_data(data, timestamp).await?;
            }
            RtmpMessageData::AmfData { raw_data } => {
                self.on_meta_data(raw_data, timestamp).await?;
            }

            _ => {}
        }
        Ok(())
    }

    pub async fn on_amf0_command_message(
        &mut self,
        stream_id: &u32,
        command_name: &Amf0ValueType,
        transaction_id: &Amf0ValueType,
        command_object: &Amf0ValueType,
        others: &mut Vec<Amf0ValueType>,
    ) -> Result<(), SessionError> {
        let empty_cmd_name = &String::new();
        let cmd_name = match command_name {
            Amf0ValueType::UTF8String(str) => str,
            _ => empty_cmd_name,
        };

        let transaction_id = match transaction_id {
            Amf0ValueType::Number(number) => number,
            _ => &0.0,
        };

        let empty_cmd_obj: IndexMap<String, Amf0ValueType> = IndexMap::new();
        let obj = match command_object {
            Amf0ValueType::Object(obj) => obj,
            _ => &empty_cmd_obj,
        };

        match cmd_name.as_str() {
            "connect" => {
                info!("[ S<-C ] [connect] ");
                self.on_connect(transaction_id, obj).await?;
            }
            "createStream" => {
                info!("[ S<-C ] [create stream] ");
                self.on_create_stream(transaction_id).await?;
            }
            "deleteStream" => {
                if !others.is_empty() {
                    let stream_id = match others.pop() {
                        Some(Amf0ValueType::Number(streamid)) => streamid,
                        _ => 0.0,
                    };

                    info!(
                        "[ S<-C ] [delete stream] app_name: {}, stream_name: {}",
                        self.app_name,
                        self.stream_name
                    );

                    self.on_delete_stream(transaction_id, &stream_id).await?;
                    self.state = ServerSessionState::DeleteStream;
                }
            }
            "play" => {
                info!(
                    "[ S<-C ] [play]  app_name: {}, stream_name: {}",
                    self.app_name,
                    self.stream_name
                );
                self.unpacketizer.session_type = config::SERVER_PULL;
                self.on_play(transaction_id, stream_id, others).await?;
            }
            "publish" => {
                self.unpacketizer.session_type = config::SERVER_PUSH;
                self.on_publish(transaction_id, stream_id, others).await?;
            }
            _ => {}
        }

        Ok(())
    }

    fn on_set_chunk_size(&mut self, chunk_size: usize) -> Result<(), SessionError> {
        info!(
            "[ S<-C ] [set chunk size]  app_name: {}, stream_name: {}, chunk size: {}",
            self.app_name,
            self.stream_name,
            chunk_size
        );
        self.unpacketizer.update_max_chunk_size(chunk_size);
        Ok(())
    }


    pub async fn on_video_data(
        &mut self,
        data: &mut BytesMut,
        timestamp: &u32,
    ) -> Result<(), SessionError> {
        let channel_data = FrameData::Video {
            timestamp: *timestamp,
            data: data.clone(),
        };

        match self.data_sender.send(channel_data) {
            Ok(_) => {}
            Err(err) => {
                log::error!("send video err: {}", err);
                return Err(SessionError::SendFrameDataErr );
            }
        }
        //
        // self.stream_handler
        //     .save_video_data(data, *timestamp)
        //     .await?;
        if let Some(h) = self.handler.as_mut() {
            h.save_video_data(data, *timestamp).await;
        }

        Ok(())
    }

    pub async fn on_audio_data(
        &mut self,
        data: &mut BytesMut,
        timestamp: &u32,
    ) -> Result<(), SessionError> {
        let channel_data = FrameData::Audio {
            timestamp: *timestamp,
            data: data.clone(),
        };

        match self.data_sender.send(channel_data) {
            Ok(_) => {}
            Err(err) => {
                log::error!("receive audio err {}", err);
                return Err(SessionError::SendFrameDataErr);
            }
        }

        if let Some(h) = self.handler.as_mut() {
            h.save_audio_data(data, *timestamp).await;
        }
        //
        // self.stream_handler
        //     .save_audio_data(data, *timestamp)
        //     .await?;

        Ok(())
    }

    pub async fn on_meta_data(
        &mut self,
        data: &mut BytesMut,
        timestamp: &u32,
    ) -> Result<(), SessionError> {
        let channel_data = FrameData::MetaData {
            timestamp: *timestamp,
            data: data.clone(),
        };

        match self.data_sender.send(channel_data) {
            Ok(_) => {}
            Err(_) => {
                return Err(SessionError::SendFrameDataErr)
            }
        }
        //
        // self.stream_handler.save_metadata(data, *timestamp).await;
        if let Some(h) = self.handler.as_mut() {
            h.save_metadata(data, *timestamp).await;
        }


        Ok(())
    }


    fn parse_connect_properties(&mut self, command_obj: &IndexMap<String, Amf0ValueType>) {
        for (property, value) in command_obj {
            match property.as_str() {
                "app" => {
                    if let Amf0ValueType::UTF8String(app) = value {
                        self.connect_properties.app = Some(app.clone());
                    }
                }
                "flashVer" => {
                    if let Amf0ValueType::UTF8String(flash_ver) = value {
                        self.connect_properties.flash_ver = Some(flash_ver.clone());
                    }
                }
                "swfUrl" => {
                    if let Amf0ValueType::UTF8String(swf_url) = value {
                        self.connect_properties.swf_url = Some(swf_url.clone());
                    }
                }
                "tcUrl" => {
                    if let Amf0ValueType::UTF8String(tc_url) = value {
                        self.connect_properties.tc_url = Some(tc_url.clone());
                    }
                }
                "fpad" => {
                    if let Amf0ValueType::Boolean(fpad) = value {
                        self.connect_properties.fpad = Some(*fpad);
                    }
                }
                "audioCodecs" => {
                    if let Amf0ValueType::Number(audio_codecs) = value {
                        self.connect_properties.audio_codecs = Some(*audio_codecs);
                    }
                }
                "videoCodecs" => {
                    if let Amf0ValueType::Number(video_codecs) = value {
                        self.connect_properties.video_codecs = Some(*video_codecs);
                    }
                }
                "videoFunction" => {
                    if let Amf0ValueType::Number(video_function) = value {
                        self.connect_properties.video_function = Some(*video_function);
                    }
                }
                "pageUrl" => {
                    if let Amf0ValueType::UTF8String(page_url) = value {
                        self.connect_properties.page_url = Some(page_url.clone());
                    }
                }
                "objectEncoding" => {
                    if let Amf0ValueType::Number(object_encoding) = value {
                        self.connect_properties.object_encoding = Some(*object_encoding);
                    }
                }
                _ => {
                    log::warn!("unknown connect properties: {}:{:?}", property, value);
                }
            }
        }
    }

    async fn on_connect(
        &mut self,
        transaction_id: &f64,
        command_obj: &IndexMap<String, Amf0ValueType>,
    ) -> Result<(), SessionError> {
        self.parse_connect_properties(command_obj);
        info!("connect properties: {:?}", self.connect_properties);
        let mut control_message =
            ProtocolControlMessagesWriter::new(AsyncBytesWriter::new(self.io.clone()));
        info!("[ S->C ] [set window_acknowledgement_size]");
        control_message
            .write_window_acknowledgement_size(define::WINDOW_ACKNOWLEDGEMENT_SIZE)
            .await?;

        info!("[ S->C ] [set set_peer_bandwidth]",);
        control_message
            .write_set_peer_bandwidth(
                define::PEER_BANDWIDTH,
                define::peer_bandwidth_limit_type::DYNAMIC,
            )
            .await?;

        let obj_encoding = command_obj.get("objectEncoding");
        let encoding = match obj_encoding {
            Some(Amf0ValueType::Number(encoding)) => encoding,
            _ => &define::OBJENCODING_AMF0,
        };

        let app_name = command_obj.get("app");
        self.app_name = match app_name {
            Some(Amf0ValueType::UTF8String(app)) => {
                // the value can weirdly have the query params, lets just remove it
                // example: live/stream?token=123
                app.split(&['?', '/']).next().unwrap_or(app).to_string()
            }
            _ => {
                return Err(SessionError::NoAppName);
            }
        };

        let mut netconnection = NetConnection::new(Arc::clone(&self.io));
        info!("[ S->C ] [set connect_response]",);
        netconnection
            .write_connect_response(
                transaction_id,
                define::FMSVER,
                &define::CAPABILITIES,
                &String::from("NetConnection.Connect.Success"),
                define::LEVEL,
                &String::from("Connection Succeeded."),
                encoding,
            )
            .await?;

        Ok(())
    }

    pub async fn on_create_stream(&mut self, transaction_id: &f64) -> Result<(), SessionError> {
        let mut netconnection = NetConnection::new(Arc::clone(&self.io));
        netconnection
            .write_create_stream_response(transaction_id, &define::STREAM_ID)
            .await?;

        info!(
            "[ S->C ] [create_stream_response]  app_name: {}",
            self.app_name,
        );

        Ok(())
    }

    pub async fn on_delete_stream(
        &mut self,
        transaction_id: &f64,
        stream_id: &f64,
    ) -> Result<(), SessionError> {
        // self.common
        //     .unpublish_to_stream_hub(self.app_name.clone(), self.stream_name.clone())
        //     .await?;
        self.notify_session_end();

        let mut netstream = NetStreamWriter::new(Arc::clone(&self.io));
        netstream
            .write_on_status(
                transaction_id,
                "status",
                "NetStream.DeleteStream.Suceess",
                "",
            )
            .await?;

        //self.unsubscribe_from_channels().await?;
        info!(
            "[ S->C ] [delete stream success]  app_name: {}, stream_name: {}",
            self.app_name,
            self.stream_name
        );
        log::trace!("{}", stream_id);

        Ok(())
    }

    fn get_request_url(&mut self, raw_stream_name: String) -> String {
        if let Some(tc_url) = &self.connect_properties.tc_url {
            format!("{tc_url}/{raw_stream_name}")
        } else {
            format!("{}/{}", self.app_name.clone(), raw_stream_name)
        }
    }

    #[allow(clippy::never_loop)]
    pub async fn on_play(
        &mut self,
        transaction_id: &f64,
        stream_id: &u32,
        other_values: &mut Vec<Amf0ValueType>,
    ) -> Result<(), SessionError> {
        let length = other_values.len() as u8;
        let mut index: u8 = 0;

        let mut stream_name: Option<String> = None;
        let mut start: Option<f64> = None;
        let mut duration: Option<f64> = None;
        let mut reset: Option<bool> = None;

        loop {
            if index >= length {
                break;
            }
            index += 1;
            stream_name = match other_values.remove(0) {
                Amf0ValueType::UTF8String(val) => Some(val),
                _ => None,
            };

            if index >= length {
                break;
            }
            index += 1;
            start = match other_values.remove(0) {
                Amf0ValueType::Number(val) => Some(val),
                _ => None,
            };

            if index >= length {
                break;
            }
            index += 1;
            duration = match other_values.remove(0) {
                Amf0ValueType::Number(val) => Some(val),
                _ => None,
            };

            if index >= length {
                break;
            }
            //index = index + 1;
            reset = match other_values.remove(0) {
                Amf0ValueType::Boolean(val) => Some(val),
                _ => None,
            };
            break;
        }

        let mut event_messages = EventMessagesWriter::new(AsyncBytesWriter::new(self.io.clone()));
        event_messages.write_stream_begin(*stream_id).await?;
        info!(
            "[ S->C ] [stream begin]  app_name: {}, stream_name: {}",
            self.app_name,
            self.stream_name
        );
        log::trace!(
            "{} {} {}",
            start.is_some(),
            duration.is_some(),
            reset.is_some()
        );

        let mut netstream = NetStreamWriter::new(Arc::clone(&self.io));
        netstream
            .write_on_status(transaction_id, "status", "NetStream.Play.Reset", "reset")
            .await?;

        netstream
            .write_on_status(
                transaction_id,
                "status",
                "NetStream.Play.Start",
                "play start",
            )
            .await?;

        netstream
            .write_on_status(
                transaction_id,
                "status",
                "NetStream.Data.Start",
                "data start.",
            )
            .await?;

        netstream
            .write_on_status(
                transaction_id,
                "status",
                "NetStream.Play.PublishNotify",
                "play publish notify.",
            )
            .await?;

        event_messages.write_stream_is_record(*stream_id).await?;

        let raw_stream_name = stream_name.unwrap();

        (self.stream_name, self.query) =
            RtmpUrlParser::parse_stream_name_with_query(&raw_stream_name);
        // if let Some(auth) = &self.auth {
        //     auth.authenticate(
        //         &self.stream_name,
        //         &self
        //             .query
        //             .as_ref()
        //             .map(|q| SecretCarrier::Query(q.to_string())),
        //         true,
        //     )?
        // }

        let query = if let Some(query_val) = &self.query {
            query_val.clone()
        } else {
            String::from("none")
        };

        info!(
            "[ S->C ] [stream is record]  app_name: {}, stream_name: {}, query: {}",
            self.app_name,
            self.stream_name,
            query
        );

        /*Now it can update the request url*/
        self.context.request_url = self.get_request_url(raw_stream_name);
        // self.common
        //     .subscribe_from_stream_hub(self.app_name.clone(), self.stream_name.clone())
        //     .await?;

        self.notify_play().await;

        self.state = ServerSessionState::Play;

        Ok(())
    }

    pub async fn on_publish(
        &mut self,
        transaction_id: &f64,
        stream_id: &u32,
        other_values: &mut Vec<Amf0ValueType>,
    ) -> Result<(), SessionError> {
        let length = other_values.len();

        if length < 2 {
            return Err(SessionError::Amf0ValueCountNotCorrect);
        }

        let stream_name_with_query = match other_values.remove(0) {
            Amf0ValueType::UTF8String(val) => val,
            _ => {
                return Err(SessionError::Amf0ValueCountNotCorrect);
            }
        };

        if !stream_name_with_query.is_empty() {
            (self.stream_name, self.query) =
                RtmpUrlParser::parse_stream_name_with_query(&stream_name_with_query);
        } else {
            log::warn!("stream_name_with_query is empty, extracing info from swf_url instead...");
            let mut url = RtmpUrlParser::new(
                self.connect_properties
                    .swf_url
                    .clone()
                    .unwrap_or("".to_string()),
            );

            match url.parse_url() {
                Ok(_) => {
                    self.stream_name = url.stream_name;
                    self.query = url.query;
                }
                Err(e) => {
                    log::warn!("Failed to parse swf_url: {e}");
                }
            }
        }
        // if let Some(auth) = &self.auth {
        //     auth.authenticate(
        //         &self.stream_name,
        //         &self
        //             .query
        //             .as_ref()
        //             .map(|q| SecretCarrier::Query(q.to_string())),
        //         false,
        //     )?
        // }

        /*Now it can update the request url*/
        self.context.request_url = self.get_request_url(stream_name_with_query);

        let _ = match other_values.remove(0) {
            Amf0ValueType::UTF8String(val) => val,
            _ => {
                return Err(SessionError::Amf0ValueCountNotCorrect);
            }
        };

        let query = if let Some(query_val) = &self.query {
            query_val.clone()
        } else {
            String::from("none")
        };

        info!(
            "[ S<-C ] [publish]  app_name: {}, stream_name: {}, query: {}",
            self.app_name,
            self.stream_name,
            query
        );

        info!(
            "[ S->C ] [stream begin]  app_name: {}, stream_name: {}, query: {}",
            self.app_name,
            self.stream_name,
            query
        );

        let mut event_messages = EventMessagesWriter::new(AsyncBytesWriter::new(self.io.clone()));
        event_messages.write_stream_begin(*stream_id).await?;

        let mut netstream = NetStreamWriter::new(Arc::clone(&self.io));
        netstream
            .write_on_status(transaction_id, "status", "NetStream.Publish.Start", "")
            .await?;
        info!(
            "[ S->C ] [NetStream.Publish.Start]  app_name: {}, stream_name: {}",
            self.app_name,
            self.stream_name
        );

        self.notify_publish().await;

        // self.common
        //     .publish_to_stream_hub(
        //         self.app_name.clone(),
        //         self.stream_name.clone(),
        //         self.gop_num,
        //     )
        //     .await?;

        Ok(())
    }
}

