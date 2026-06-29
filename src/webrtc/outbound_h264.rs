use anyhow::{anyhow, Result};
use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocalWriter;

use super::h264_util::{contains_sps_or_pps_nalu, ensure_annex_b};

/// Sends H264 Annex B access units via TrackLocalStaticSample (webrtc-rs recommended path).
pub struct OutboundH264Track {
    track: Arc<TrackLocalStaticSample>,
}

impl OutboundH264Track {
    pub fn new(track: Arc<TrackLocalStaticSample>) -> Self {
        Self { track }
    }

    pub async fn wait_binding(&self, label: &str) -> Result<()> {
        // Packetizer is created on bind during SDP negotiation.
        tokio::time::sleep(Duration::from_millis(300)).await;
        info!("[WebRTC] {} sample track ready", label);
        Ok(())
    }

    /// Send one complete H264 access unit (Annex B).
    pub async fn send_access_unit(&self, annex_b: &[u8], duration: Duration) -> Result<()> {
        if annex_b.is_empty() {
            return Ok(());
        }
        let sample = Sample {
            data: Bytes::copy_from_slice(annex_b),
            duration,
            ..Default::default()
        };
        self.track
            .write_sample(&sample)
            .await
            .map_err(|e| anyhow!("write_sample: {}", e))
    }
}

/// Build Annex B with SPS + PPS + slice/IDR for reliable decoder startup.
pub fn annex_b_with_config(sps: &[u8], pps: &[u8], access_unit: &[u8]) -> Vec<u8> {
    let au = ensure_annex_b(access_unit);
    if contains_sps_or_pps_nalu(&au) {
        return au;
    }
    let mut out = Vec::new();
    for nalu in [sps, pps] {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nalu);
    }
    out.extend_from_slice(&au);
    out
}
