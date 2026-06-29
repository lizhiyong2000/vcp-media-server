use anyhow::Result;
use std::sync::Arc;
use tracing::info;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::api::API;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::RTCPeerConnection;

pub fn create_api() -> Result<Arc<API>> {
    let mut media_engine = MediaEngine::default();
    media_engine.register_default_codecs()?;

    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut media_engine)?;

    let api = APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build();

    Ok(Arc::new(api))
}

pub fn peer_config() -> RTCConfiguration {
    RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    }
}

pub async fn new_peer_connection(api: &Arc<API>) -> Result<Arc<RTCPeerConnection>> {
    Ok(Arc::new(api.new_peer_connection(peer_config()).await?))
}

pub fn wire_pc_debug(pc: Arc<RTCPeerConnection>, label: &'static str) {
    pc.on_peer_connection_state_change(Box::new(move |state| {
        let label = label;
        Box::pin(async move {
            info!("[WebRTC] {} PC state -> {:?}", label, state);
        })
    }));
    pc.on_ice_connection_state_change(Box::new(move |state| {
        let label = label;
        Box::pin(async move {
            info!("[WebRTC] {} ICE state -> {:?}", label, state);
        })
    }));
    pc.on_ice_gathering_state_change(Box::new(move |state| {
        let label = label;
        Box::pin(async move {
            info!("[WebRTC] {} ICE gathering -> {:?}", label, state);
        })
    }));
}
