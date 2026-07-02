//! Parse H264 bitstream from RTP payloads (RFC 6184), independent of Annex-B splitting.

use bytes::{Bytes, BytesMut};

const STAP_A: u8 = 24;
const FU_A: u8 = 28;

#[derive(Debug, Clone)]
pub struct ParsedNalu {
    pub nal_type: u8,
    pub data: Vec<u8>, // raw NALU including 1-byte header
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264DepacketizeError {
    ShortPacket,
    StapASizeLargerThanBuffer,
    MissingFuStart,
    UnsupportedNaluType(u8),
}

#[derive(Debug, Default)]
pub struct H264RtpDepacketizer {
    fu_buffer: Option<BytesMut>,
}

impl H264RtpDepacketizer {
    pub fn push(&mut self, payload: &[u8]) -> Result<Option<Bytes>, H264DepacketizeError> {
        if payload.is_empty() {
            return Ok(None);
        }
        let nal_type = payload[0] & 0x1F;
        match nal_type {
            1..=23 => Ok(Some(nalus_to_annex_b(&[ParsedNalu {
                nal_type,
                data: payload.to_vec(),
            }]))),
            STAP_A => {
                let nalus = parse_stap_a_checked(payload)?;
                Ok(Some(nalus_to_annex_b(&nalus)))
            }
            FU_A => self.push_fu_a(payload),
            _ => Err(H264DepacketizeError::UnsupportedNaluType(nal_type)),
        }
    }

    pub fn reset(&mut self) {
        self.fu_buffer = None;
    }

    fn push_fu_a(&mut self, payload: &[u8]) -> Result<Option<Bytes>, H264DepacketizeError> {
        if payload.len() < 2 {
            self.reset();
            return Err(H264DepacketizeError::ShortPacket);
        }

        let fu_indicator = payload[0];
        let fu_header = payload[1];
        let start = (fu_header & 0x80) != 0;
        let end = (fu_header & 0x40) != 0;
        let fragmented_nal_type = fu_header & 0x1F;

        if start {
            let mut buffer = BytesMut::new();
            buffer.extend_from_slice(&[0, 0, 0, 1]);
            buffer.extend_from_slice(&[(fu_indicator & 0xE0) | fragmented_nal_type]);
            buffer.extend_from_slice(&payload[2..]);
            self.fu_buffer = Some(buffer);
        } else if self.fu_buffer.is_none() {
            return Err(H264DepacketizeError::MissingFuStart);
        } else if let Some(buffer) = &mut self.fu_buffer {
            buffer.extend_from_slice(&payload[2..]);
        }

        if end {
            return Ok(self.fu_buffer.take().map(|buffer| buffer.freeze()));
        }
        Ok(None)
    }
}

/// Parse one RTP H264 payload into NALUs (complete only).
pub fn parse_rtp_h264(payload: &[u8]) -> Vec<ParsedNalu> {
    if payload.is_empty() {
        return Vec::new();
    }
    let b0 = payload[0];
    let nal_type = b0 & 0x1F;

    match nal_type {
        1..=23 => {
            vec![ParsedNalu {
                nal_type,
                data: payload.to_vec(),
            }]
        }
        STAP_A => parse_stap_a(payload),
        FU_A => Vec::new(), // handled by depacketizer state machine
        _ => Vec::new(),
    }
}

fn parse_stap_a(payload: &[u8]) -> Vec<ParsedNalu> {
    parse_stap_a_checked(payload).unwrap_or_default()
}

fn parse_stap_a_checked(payload: &[u8]) -> Result<Vec<ParsedNalu>, H264DepacketizeError> {
    let mut out = Vec::new();
    let mut i = 1; // skip STAP-A header
    while i + 2 <= payload.len() {
        let nalu_len = ((payload[i] as usize) << 8) | payload[i + 1] as usize;
        i += 2;
        if nalu_len == 0 || i + nalu_len > payload.len() {
            return Err(H264DepacketizeError::StapASizeLargerThanBuffer);
        }
        let nalu = &payload[i..i + nalu_len];
        if !nalu.is_empty() {
            out.push(ParsedNalu {
                nal_type: nalu[0] & 0x1F,
                data: nalu.to_vec(),
            });
        }
        i += nalu_len;
    }
    Ok(out)
}

pub fn nalus_to_annex_b(nalus: &[ParsedNalu]) -> Bytes {
    let mut out = BytesMut::new();
    for n in nalus {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&n.data);
    }
    out.freeze()
}

pub fn annex_b_from_rtp_payload(payload: &[u8]) -> Option<Bytes> {
    let nalus = parse_rtp_h264(payload);
    if nalus.is_empty() {
        return None;
    }
    Some(nalus_to_annex_b(&nalus))
}

pub fn contains_idr(nalus: &[ParsedNalu]) -> bool {
    nalus.iter().any(|n| n.nal_type == 5)
}

/// Detect IDR from RTP payload before full FU-A reassembly.
pub fn is_idr_rtp_payload(payload: &[u8]) -> bool {
    if payload.is_empty() {
        return false;
    }
    let nal_type = payload[0] & 0x1F;
    match nal_type {
        5 => true,
        STAP_A => contains_idr(&parse_stap_a(payload)),
        FU_A if payload.len() >= 2 => (payload[1] & 0x1F) == 5,
        _ => false,
    }
}

pub fn extract_sps_pps_from_nalus(nalus: &[ParsedNalu]) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut sps = None;
    let mut pps = None;
    for n in nalus {
        match n.nal_type {
            7 => sps = Some(n.data.clone()),
            8 => pps = Some(n.data.clone()),
            _ => {}
        }
    }
    (sps, pps)
}

pub fn describe_rtp_payload(payload: &[u8]) -> String {
    let nalus = parse_rtp_h264(payload);
    if nalus.is_empty() {
        let b0 = payload.first().copied().unwrap_or(0);
        let t = b0 & 0x1F;
        if t == FU_A {
            let inner = payload.get(1).copied().unwrap_or(0) & 0x1F;
            return format!("fua-frag(type={})", inner);
        }
        return format!("raw:{}B b0=0x{:02x}", payload.len(), b0);
    }
    nalus
        .iter()
        .map(|n| {
            format!(
                "{}({})",
                super::h264_util::nalu_type_name(n.nal_type),
                n.data.len()
            )
        })
        .collect::<Vec<_>>()
        .join("+")
}

pub fn is_fu_a_continuation(payload: &[u8]) -> bool {
    if payload.len() < 2 || (payload[0] & 0x1F) != FU_A {
        return false;
    }
    let start = (payload[1] & 0x80) != 0;
    !start
}

pub fn hex_prefix(payload: &[u8], n: usize) -> String {
    payload
        .iter()
        .take(n)
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stap_a_sps_pps() {
        let sps = [0x67u8, 0x42, 0x00, 0x1f];
        let pps = [0x68u8, 0xce, 0x38, 0x80];
        let mut stap = vec![0x78u8];
        stap.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        stap.extend_from_slice(&sps);
        stap.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        stap.extend_from_slice(&pps);

        let nalus = parse_rtp_h264(&stap);
        assert_eq!(nalus.len(), 2);
        assert_eq!(nalus[0].nal_type, 7);
        assert_eq!(nalus[1].nal_type, 8);
        let (s, p) = extract_sps_pps_from_nalus(&nalus);
        assert!(s.is_some() && p.is_some());
    }

    #[test]
    fn single_slice() {
        let payload = [0x41u8, 0x9a, 0x12];
        let nalus = parse_rtp_h264(&payload);
        assert_eq!(nalus.len(), 1);
        assert_eq!(nalus[0].nal_type, 1);
    }

    #[test]
    fn fu_a_middle_packets_are_pending_until_end() {
        let mut depacketizer = H264RtpDepacketizer::default();

        assert_eq!(depacketizer.push(&[0x7c, 0x85, 0x88]).unwrap(), None);
        assert_eq!(depacketizer.push(&[0x7c, 0x05, 0x99]).unwrap(), None);
        let annex_b = depacketizer
            .push(&[0x7c, 0x45, 0xaa])
            .unwrap()
            .expect("end fragment should complete NAL");

        assert_eq!(&annex_b[..], &[0, 0, 0, 1, 0x65, 0x88, 0x99, 0xaa]);
    }

    #[test]
    fn fu_a_continuation_without_start_is_rejected() {
        let mut depacketizer = H264RtpDepacketizer::default();

        assert_eq!(
            depacketizer.push(&[0x7c, 0x05, 0x99]),
            Err(H264DepacketizeError::MissingFuStart)
        );
    }
}
