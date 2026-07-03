use crate::core::Track;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterleavedChannel {
    pub track_id: u8,
    pub rtp_channel: u8,
    pub rtcp_channel: u8,
}

#[derive(Debug)]
pub struct RtspSession {
    pub stream_id: Option<String>,
    pub session_id: Option<String>,
    pub playing: bool,
    /// True after ANNOUNCE/RECORD — UDP SETUP should ingest RTP from client.
    pub publishing: bool,
    pub publisher_id: Option<String>,
    pub transport_mode: TransportMode,
    pub interleaved_channels: Vec<InterleavedChannel>,
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
            publishing: false,
            publisher_id: None,
            transport_mode: TransportMode::Tcp,
            interleaved_channels: Vec::new(),
            tracks: Vec::new(),
            rtp_task_started: false,
            sps: None,
            pps: None,
        }
    }

    pub fn rtp_track_for_channel(&self, channel: u8) -> Option<u8> {
        self.interleaved_channels
            .iter()
            .find(|entry| entry.rtp_channel == channel)
            .map(|entry| entry.track_id)
    }

    pub fn is_rtcp_channel(&self, channel: u8) -> bool {
        self.interleaved_channels
            .iter()
            .any(|entry| entry.rtcp_channel == channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_channel_mapping_distinguishes_rtp_and_rtcp() {
        let mut session = RtspSession::new();
        session.interleaved_channels.push(InterleavedChannel {
            track_id: 0,
            rtp_channel: 4,
            rtcp_channel: 5,
        });
        session.interleaved_channels.push(InterleavedChannel {
            track_id: 1,
            rtp_channel: 6,
            rtcp_channel: 7,
        });

        assert_eq!(session.rtp_track_for_channel(4), Some(0));
        assert_eq!(session.rtp_track_for_channel(6), Some(1));
        assert_eq!(session.rtp_track_for_channel(5), None);
        assert!(session.is_rtcp_channel(5));
        assert!(session.is_rtcp_channel(7));
        assert!(!session.is_rtcp_channel(8));
    }
}
