mod h264_util;
mod h264_rtp_ingest;
mod outbound_h264;
mod peer;
mod rtp_h264;
mod sdp_h264;
pub use sdp_h264::parse_sprop_parameter_sets;
mod play_relay;
mod player;
mod publisher;
mod publish_signaling;
mod signaling;

pub use h264_rtp_ingest::{H264RtpIngest, rtp_h264_media_payload};

use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use tracing::{debug, error, info, warn};
use webrtc::peer_connection::RTCPeerConnection;

use crate::core::StreamManager;
use peer::create_api;
use player::{cancel_play_relay, start_play};
use publisher::{add_ice_candidate, start_publish};
use publish_signaling::{register_publish_signaling, request_publisher_keyframe, unregister_publish_signaling};
use signaling::{ClientSignal, ServerSignal};

pub struct WebrtcServer {
    stream_manager: Arc<StreamManager>,
    port: u16,
}

/// One WebSocket may hold both publish and play peer connections simultaneously.
struct SessionState {
    publish_pc: Option<Arc<RTCPeerConnection>>,
    play_pc: Option<Arc<RTCPeerConnection>>,
    stream_id: Option<String>,
    play_relay_id: Option<String>,
}

impl SessionState {
    fn has_publish(&self) -> bool {
        self.publish_pc.is_some()
    }
}

impl WebrtcServer {
    pub fn new(stream_manager: Arc<StreamManager>, port: u16) -> Self {
        Self { stream_manager, port }
    }

    pub async fn start(&self) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.port);
        info!("[WebRTC] Initializing WebRTC signaling server on {}", addr);

        let api = create_api()?;
        let listener = TcpListener::bind(&addr).await?;
        info!("[WebRTC] WebRTC signaling server ready on {}", addr);
        info!("[WebRTC] WebSocket: ws://127.0.0.1:{}/", self.port);
        info!("[WebRTC] Signals: publish, play, ice");

        loop {
            match listener.accept().await {
                Ok((socket, peer_addr)) => {
                    info!("[WebRTC] New connection from {}", peer_addr);
                    let manager = self.stream_manager.clone();
                    let api = api.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(socket, manager, api).await {
                            error!("[WebRTC] Connection error from {}: {}", peer_addr, e);
                        }
                    });
                }
                Err(e) => {
                    error!("[WebRTC] Accept error: {}", e);
                }
            }
        }
    }
}

async fn handle_connection(
    stream: TcpStream,
    manager: Arc<StreamManager>,
    api: Arc<webrtc::api::API>,
) -> Result<()> {
    let ws = accept_async(stream).await?;
    let (mut ws_tx, mut ws_rx) = ws.split();

    let (ice_tx, mut ice_rx) = mpsc::unbounded_channel::<ServerSignal>();
    let mut state = SessionState {
        publish_pc: None,
        play_pc: None,
        stream_id: None,
        play_relay_id: None,
    };

    loop {
        tokio::select! {
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(e) = handle_signal(&text, &api, &manager, &mut state, &ice_tx, &mut ws_tx).await {
                            warn!("[WebRTC] Signal error: {}", e);
                            let err = ServerSignal::Error { message: e.to_string() };
                            let _ = ws_tx.send(Message::Text(err.to_json())).await;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        warn!("[WebRTC] WebSocket read error: {}", e);
                        break;
                    }
                }
            }
            ice = ice_rx.recv() => {
                if let Some(signal) = ice {
                    let _ = ws_tx.send(Message::Text(signal.to_json())).await;
                }
            }
        }
    }

    cleanup_session(&manager, &mut state).await;
    Ok(())
}

async fn cleanup_session(manager: &Arc<StreamManager>, state: &mut SessionState) {
    if let Some(relay_id) = state.play_relay_id.take() {
        cancel_play_relay(&relay_id);
    }
    if let Some(pc) = state.play_pc.take() {
        let _ = pc.close().await;
    }
    if state.has_publish() {
        let sid = state
            .stream_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let pc = state.publish_pc.take();
        cleanup_publish_session(manager, &sid, pc).await;
    }
}

async fn cleanup_publish_session(
    manager: &Arc<StreamManager>,
    stream_id: &str,
    pc: Option<Arc<RTCPeerConnection>>,
) {
    if let Some(pc) = pc {
        let _ = pc.close().await;
    }
    unregister_publish_signaling(stream_id);
    let _ = manager.set_unpublished(stream_id);
    info!(
        "[WebRTC] Publish session cleaned up stream='{}'",
        stream_id
    );
}

async fn route_ice_candidate(
    state: &SessionState,
    candidate: String,
    sdp_mid: Option<String>,
    sdp_mline_index: Option<u16>,
) -> Result<()> {
    let mut applied = false;

    if let Some(pc) = &state.publish_pc {
        if add_ice_candidate(pc, candidate.clone(), sdp_mid.clone(), sdp_mline_index).await.is_ok()
        {
            applied = true;
        }
    }
    if let Some(pc) = &state.play_pc {
        if add_ice_candidate(pc, candidate, sdp_mid, sdp_mline_index).await.is_ok() {
            applied = true;
        }
    }

    if !applied {
        warn!("[WebRTC] ICE candidate did not match any PC on this connection");
    }
    Ok(())
}

async fn handle_signal<S>(
    text: &str,
    api: &Arc<webrtc::api::API>,
    manager: &Arc<StreamManager>,
    state: &mut SessionState,
    ice_tx: &mpsc::UnboundedSender<ServerSignal>,
    ws_tx: &mut S,
) -> Result<()>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let signal: ClientSignal = serde_json::from_str(text)
        .map_err(|e| anyhow!("invalid signal JSON: {}", e))?;

    match &signal {
        ClientSignal::Publish { stream_id, .. } => {
            debug!("[WebRTC] WS signal publish stream='{}'", stream_id);
        }
        ClientSignal::Play { stream_id, .. } => {
            debug!("[WebRTC] WS signal play stream='{}'", stream_id);
        }
        ClientSignal::StopPlay { stream_id } => {
            debug!("[WebRTC] WS signal stop_play stream='{}'", stream_id);
        }
        ClientSignal::StopPublish { stream_id } => {
            debug!("[WebRTC] WS signal stop_publish stream='{}'", stream_id);
        }
        ClientSignal::Ice { candidate, .. } => {
            debug!(
                "[WebRTC] WS signal ice cand={}",
                &candidate[..candidate.len().min(48)]
            );
        }
    }

    match signal {
        ClientSignal::Publish { stream_id, sdp } => {
            info!("[WebRTC] Publish request stream='{}'", stream_id);
            if let Some(old_pc) = state.publish_pc.take() {
                let _ = old_pc.close().await;
            }
            state.stream_id = Some(stream_id.clone());
            register_publish_signaling(&stream_id, ice_tx.clone());
            let session = start_publish(
                api.clone(),
                manager.clone(),
                stream_id,
                sdp,
                ice_tx.clone(),
            )
            .await?;

            state.publish_pc = Some(session.pc);
            let answer = ServerSignal::Answer {
                sdp: session.answer_sdp,
            };
            ws_tx.send(Message::Text(answer.to_json())).await?;
        }
        ClientSignal::Play { stream_id, sdp } => {
            info!("[WebRTC] Play request stream='{}'", stream_id);
            if let Some(relay_id) = state.play_relay_id.take() {
                cancel_play_relay(&relay_id);
            }
            if let Some(old_pc) = state.play_pc.take() {
                let _ = old_pc.close().await;
            }
            let _ = request_publisher_keyframe(&stream_id);
            if state.stream_id.is_none() {
                state.stream_id = Some(stream_id.clone());
            }
            let session = start_play(
                api.clone(),
                manager.clone(),
                stream_id,
                sdp,
                ice_tx.clone(),
            )
            .await?;

            state.play_pc = Some(session.pc);
            state.play_relay_id = Some(session.relay_id);
            let answer = ServerSignal::Answer {
                sdp: session.answer_sdp,
            };
            ws_tx.send(Message::Text(answer.to_json())).await?;
        }
        ClientSignal::StopPlay { stream_id: _ } => {
            info!("[WebRTC] Stop play");
            if let Some(relay_id) = state.play_relay_id.take() {
                cancel_play_relay(&relay_id);
            }
            if let Some(pc) = state.play_pc.take() {
                let _ = pc.close().await;
            }
        }
        ClientSignal::StopPublish { stream_id } => {
            info!("[WebRTC] Stop publish stream='{}'", stream_id);
            let pc = state.publish_pc.take();
            cleanup_publish_session(manager, &stream_id, pc).await;
            if state.play_pc.is_none() {
                state.stream_id = None;
            }
        }
        ClientSignal::Ice {
            candidate,
            sdp_mid,
            sdp_mline_index,
        } => {
            route_ice_candidate(state, candidate, sdp_mid, sdp_mline_index).await?;
        }
    }

    Ok(())
}
