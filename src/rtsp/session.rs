use crate::core::Track;

#[derive(Debug)]
pub struct RtspSession {
    pub stream_id: Option<String>,
    pub session_id: Option<String>,
    pub playing: bool,
    pub transport_mode: TransportMode,
    pub interleaved_channels: Vec<(u16, u16)>,
    pub tracks: Vec<Track>,
    pub rtp_task_started: bool,
    // Codec parameters from SDP
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransportMode {
    Tcp,
    Udp,
}

impl Default for TransportMode {
    fn default() -> Self {
        TransportMode::Tcp
    }
}

impl RtspSession {
    pub fn new() -> Self {
        Self {
            stream_id: None,
            session_id: None,
            playing: false,
            transport_mode: TransportMode::Tcp,
            interleaved_channels: Vec::new(),
            tracks: Vec::new(),
            rtp_task_started: false,
            sps: None,
            pps: None,
        }
    }
}