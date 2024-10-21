
use byteorder::BigEndian;
use bytes::BytesMut;

use rand::Rng;
use std::sync::Arc;
use tokio::sync::Mutex;


use vcp_media_common::Marshal;
use vcp_media_common::Unmarshal;


use vcp_media_common::bytesio::bytes_errors::BytesWriteError;
use vcp_media_common::bytesio::bytes_reader::BytesReader;
use vcp_media_common::bytesio::bytes_writer::AsyncBytesWriter;
use vcp_media_common::bytesio::bytesio;
use vcp_media_common::bytesio::bytesio::TNetIO;


use vcp_media_rtp::errors::PackerError;
use vcp_media_rtp::errors::UnPackerError;
use vcp_media_rtp::rtcp::rtcp_header::RtcpHeader;
use vcp_media_rtp::rtcp::RTCP_RR;
use vcp_media_rtp::rtcp::RTCP_SR;
use vcp_media_rtp::utils::OnFrameFn;
use vcp_media_rtp::utils::OnRtpPacketFn;
use vcp_media_rtp::utils::OnRtpPacketFn2;
use vcp_media_rtp::RtpPacket;

use vcp_media_rtp::rtp_aac::RtpAacPacker;
use vcp_media_rtp::rtp_h264::RtpH264Packer;
use vcp_media_rtp::rtp_h265::RtpH265Packer;

use vcp_media_rtp::rtp_aac::RtpAacUnPacker;
use vcp_media_rtp::rtp_h264::RtpH264UnPacker;
use vcp_media_rtp::rtp_h265::RtpH265UnPacker;

use vcp_media_rtp::rtcp::rtcp_context::RtcpContext;
use vcp_media_rtp::rtcp::rtcp_sr::RtcpSenderReport;
use vcp_media_rtp::utils::TPacker;
use vcp_media_rtp::utils::TUnPacker;


use super::rtsp_codec::RtspCodecId;
use super::rtsp_codec::RtspCodecInfo;



pub trait TRtpFunc {
    fn create_packer(&mut self, writer: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>);
    fn create_unpacker(&mut self);
}

pub struct RtpChannel {
    codec_info: RtspCodecInfo,
    pub rtp_packer: Option<Box<dyn TPacker>>,
    pub rtp_unpacker: Option<Box<dyn TUnPacker>>,
    ssrc: u32,
    init_sequence: u16,
}

#[derive(Default)]
pub struct RtcpChannel {
    recv_ctx: RtcpContext,
    pub send_ctx: RtcpContext,
    channel_identifier: u8,
}

impl RtpChannel {
    pub fn new(codec_info: RtspCodecInfo) -> Self {
        let ssrc: u32 = rand::thread_rng().gen();
        let mut rtp_channel = RtpChannel {
            codec_info,
            ssrc,
            rtp_packer: None,
            rtp_unpacker: None,
            init_sequence: 0,
        };
        rtp_channel.create_unpacker();
        rtp_channel
    }

    //Receive av frame from network -> pack AV frame to RTP packet -> send to stream hub
    pub fn on_packet(&mut self, reader: &mut BytesReader) -> Result<(), UnPackerError> {
        if let Some(unpacker) = &mut self.rtp_unpacker {
            unpacker.unpack(reader)?;
        }
        Ok(())
    }

    //Receive av frame from stream hub -> pack -> send out
    pub async fn on_frame(
        &mut self,
        nalus: &mut BytesMut,
        timestamp: u32,
    ) -> Result<(), PackerError> {
        if let Some(packer) = &mut self.rtp_packer {
            return packer.pack(nalus, timestamp).await;
        }
        Ok(())
    }

    //Set handler for processing AV frame when unpack a whole AV frame
    //from rtp packets received from network.
    pub fn on_frame_handler(&mut self, f: OnFrameFn) {
        if let Some(unpacker) = &mut self.rtp_unpacker {
            unpacker.on_frame_handler(f);
        }
    }

    //Set handler for processing rtp packet when packed a rtp packet
    pub fn on_packet_handler(&mut self, f: OnRtpPacketFn) {
        if let Some(packer) = &mut self.rtp_packer {
            packer.on_packet_handler(f);
        }
    }

    //Set handler for processing received AV rtp packet from network
    pub fn on_packet_for_rtcp_handler(&mut self, f: OnRtpPacketFn2) {
        if let Some(packer) = &mut self.rtp_packer {
            packer.on_packet_for_rtcp_handler(f);
        }
    }
}

impl TRtpFunc for RtpChannel {
    fn create_unpacker(&mut self) {
        match self.codec_info.codec_id {
            RtspCodecId::H264 => {
                self.rtp_unpacker = Some(Box::new(RtpH264UnPacker::new()));
            }
            RtspCodecId::H265 => {
                self.rtp_unpacker = Some(Box::new(RtpH265UnPacker::new()));
            }
            RtspCodecId::AAC => {
                self.rtp_unpacker = Some(Box::new(RtpAacUnPacker::new()));
            }
            RtspCodecId::G711A => {}
        }
    }
    fn create_packer(&mut self, io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>) {
        match self.codec_info.codec_id {
            RtspCodecId::H264 => {
                self.rtp_packer = Some(Box::new(RtpH264Packer::new(
                    self.codec_info.payload_type,
                    self.ssrc,
                    self.init_sequence,
                    1400,
                    io,
                )));
            }
            RtspCodecId::H265 => {
                self.rtp_packer = Some(Box::new(RtpH265Packer::new(
                    self.codec_info.payload_type,
                    self.ssrc,
                    self.init_sequence,
                    1400,
                    io,
                )));
            }
            RtspCodecId::AAC => {
                self.rtp_packer = Some(Box::new(RtpAacPacker::new(
                    self.codec_info.payload_type,
                    self.ssrc,
                    self.init_sequence,
                    io,
                )));
            }
            RtspCodecId::G711A => {}
        }
    }
}

impl RtcpChannel {
    pub fn set_channel_identifier(&mut self, channel_idendifier: u8) {
        self.channel_identifier = channel_idendifier;
    }

    pub async fn on_rtcp(
        &mut self,
        reader: &mut BytesReader,
        rtcp_io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>,
    ) {
        let mut reader_clone = BytesReader::new(reader.get_remaining_bytes());
        if let Ok(rtcp_header) = RtcpHeader::unmarshal(&mut reader_clone) {
            match rtcp_header.payload_type {
                RTCP_SR => {
                    if let Ok(sr) = RtcpSenderReport::unmarshal(reader) {
                        self.recv_ctx.received_sr(&sr);
                        if let Err(err) = self.send_rr(rtcp_io).await {
                            log::error!("send rr error: {}", err);
                        }
                    }
                }
                RTCP_RR => {}
                _ => {}
            }
        }
    }

    pub fn on_packet(&mut self, packet: RtpPacket) {
        self.recv_ctx.received_rtp(packet);
    }

    pub async fn send_rr(
        &mut self,
        rtcp_io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>,
    ) -> Result<(), BytesWriteError> {
        let rr = self.recv_ctx.generate_rr();

        let net_type = rtcp_io.lock().await.get_net_type();
        if let Ok(msg) = rr.marshal() {
            let mut bytes_writer = AsyncBytesWriter::new(rtcp_io);
            match net_type {
                bytesio::NetType::TCP => {
                    bytes_writer.write_u8(0x24)?;
                    bytes_writer.write_u8(self.channel_identifier)?;
                    bytes_writer.write_u16::<BigEndian>(msg.len() as u16)?;
                }
                bytesio::NetType::UDP => {}
            }
            bytes_writer.write(&msg)?;
            bytes_writer.flush().await?;
        }
        Ok(())
    }
}
