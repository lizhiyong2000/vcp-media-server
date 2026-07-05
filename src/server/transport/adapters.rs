use anyhow::Result;
use std::sync::Arc;

use crate::core::{
    MediaTransport, StreamManager, StreamProtocol, StreamPuller, TransportConfig,
    TransportStatus,
};
use crate::server::rtmp::puller::RtmpPuller;
use crate::server::rtsp::RtspPuller;

pub struct TransportAdapterFactory;

impl TransportAdapterFactory {
    pub fn create_puller(config: TransportConfig) -> Box<dyn StreamPuller> {
        match config.protocol {
            StreamProtocol::RTMP => Box::new(
                RtmpPuller::new(config.stream_manager)
                    .with_url(config.remote_url.as_deref().unwrap_or(""), &config.stream_id)
            ),
            StreamProtocol::RTSP => Box::new(
                RtspPuller::new(config.stream_manager)
                    .with_url(config.remote_url.as_deref().unwrap_or(""), &config.stream_id)
            ),
            _ => panic!("Unsupported protocol"),
        }
    }
}
