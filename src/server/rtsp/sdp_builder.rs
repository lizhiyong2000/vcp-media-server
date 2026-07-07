use base64::Engine;
use webrtc::sdp::description::common::{Address, Attribute, ConnectionInformation};
use webrtc::sdp::description::media::{MediaDescription, MediaName, RangedPort};
use webrtc::sdp::description::session::{Origin, SessionDescription, TimeDescription, Timing};

use crate::core::{CodecType, Track};

#[derive(Debug, Clone)]
pub struct HevcSpsInfo {
    pub profile_space: u8,
    pub profile_idc: u8,
    pub tier_flag: u8,
    pub interop_constraints: String,
    pub level_idc: u8,
}

impl HevcSpsInfo {
    pub fn from_sps(sps_data: &[u8]) -> Option<Self> {
        if sps_data.len() < 15 {
            return None;
        }

        let rbsp_start = 2;
        let byte3 = sps_data[rbsp_start + 1];
        let profile_space = (byte3 >> 6) & 0x03;
        let tier_flag = (byte3 >> 5) & 0x01;
        let profile_idc = byte3 & 0x1F;

        let interop_constraints_bytes = &sps_data[rbsp_start + 6..rbsp_start + 12];
        let interop_constraints = interop_constraints_bytes
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<String>();

        let level_idc = sps_data[rbsp_start + 12];

        Some(Self {
            profile_space,
            profile_idc,
            tier_flag,
            interop_constraints,
            level_idc,
        })
    }
}

pub struct RtspSdpBuilder {
    session: SessionDescription,
}

impl RtspSdpBuilder {
    pub fn new(session_name: &str) -> Self {
        let origin = Origin {
            username: "-".to_string(),
            session_id: 0,
            session_version: 0,
            network_type: "IN".to_string(),
            address_type: "IP4".to_string(),
            unicast_address: "127.0.0.1".to_string(),
        };

        let time_description = TimeDescription {
            timing: Timing {
                start_time: 0,
                stop_time: 0,
            },
            repeat_times: Vec::new(),
        };

        let mut session = SessionDescription {
            version: 0,
            origin,
            session_name: session_name.to_string(),
            session_information: None,
            uri: None,
            email_address: None,
            phone_number: None,
            connection_information: None,
            bandwidth: Vec::new(),
            time_descriptions: vec![time_description],
            time_zones: Vec::new(),
            encryption_key: None,
            attributes: Vec::new(),
            media_descriptions: Vec::new(),
        };

        Self { session }
    }

    pub fn add_video_track(
        &mut self,
        codec: CodecType,
        payload_type: u8,
        clock_rate: u32,
        control: &str,
        fmtp: Option<String>,
    ) -> &mut Self {
        let codec_name = match codec {
            CodecType::H264 => "H264",
            CodecType::H265 => "H265",
            _ => "H264",
        };

        let connection_info = ConnectionInformation {
            network_type: "IN".to_string(),
            address_type: "IP4".to_string(),
            address: Some(Address {
                address: "0.0.0.0".to_string(),
                ttl: None,
                range: None,
            }),
        };

        let mut attributes = vec![
            Attribute {
                key: "rtpmap".to_string(),
                value: Some(format!("{} {}/{}", payload_type, codec_name, clock_rate)),
            },
            Attribute {
                key: "control".to_string(),
                value: Some(control.to_string()),
            },
        ];

        if let Some(fmtp_value) = fmtp {
            attributes.push(Attribute {
                key: "fmtp".to_string(),
                value: Some(format!("{} {}", payload_type, fmtp_value)),
            });
        }

        let media_name = MediaName {
            media: "video".to_string(),
            port: RangedPort {
                value: 0,
                range: None,
            },
            protos: vec!["RTP".to_string(), "AVP".to_string()],
            formats: vec![payload_type.to_string()],
        };

        let md = MediaDescription {
            media_name,
            media_title: None,
            connection_information: Some(connection_info),
            bandwidth: Vec::new(),
            encryption_key: None,
            attributes,
        };

        self.session.media_descriptions.push(md);
        self
    }

    pub fn add_audio_track(
        &mut self,
        codec: CodecType,
        payload_type: u8,
        clock_rate: u32,
        channels: u32,
        control: &str,
        fmtp: Option<String>,
    ) -> &mut Self {
        let codec_name = match codec {
            CodecType::AAC => "mpeg4-generic",
            CodecType::Opus => "opus",
            CodecType::G711 => "PCMU",
            _ => "mpeg4-generic",
        };

        let connection_info = ConnectionInformation {
            network_type: "IN".to_string(),
            address_type: "IP4".to_string(),
            address: Some(Address {
                address: "0.0.0.0".to_string(),
                ttl: None,
                range: None,
            }),
        };

        let mut attributes = vec![
            Attribute {
                key: "rtpmap".to_string(),
                value: Some(format!(
                    "{} {}/{}/{}",
                    payload_type, codec_name, clock_rate, channels
                )),
            },
            Attribute {
                key: "control".to_string(),
                value: Some(control.to_string()),
            },
        ];

        if let Some(fmtp_value) = fmtp {
            attributes.push(Attribute {
                key: "fmtp".to_string(),
                value: Some(format!("{} {}", payload_type, fmtp_value)),
            });
        }

        let media_name = MediaName {
            media: "audio".to_string(),
            port: RangedPort {
                value: 0,
                range: None,
            },
            protos: vec!["RTP".to_string(), "AVP".to_string()],
            formats: vec![payload_type.to_string()],
        };

        let md = MediaDescription {
            media_name,
            media_title: None,
            connection_information: Some(connection_info),
            bandwidth: Vec::new(),
            encryption_key: None,
            attributes,
        };

        self.session.media_descriptions.push(md);
        self
    }

    pub fn build(&self) -> String {
        self.session.to_string()
    }
}

pub fn build_sdp_for_tracks(tracks: &[Track]) -> String {
    build_sdp_for_stream("", tracks, None, None, None)
}

pub fn build_sdp_with_codec_params(
    stream_id: &str,
    video_codec: CodecType,
    vps: Option<&[u8]>,
    sps: Option<&[u8]>,
    pps: Option<&[u8]>,
) -> String {
    let session_name = format!("MediaServer Session: {}", stream_id);
    let mut builder = RtspSdpBuilder::new(&session_name);

    let control_video = "trackID=0".to_string();
    let control_audio = "trackID=1".to_string();

    if video_codec == CodecType::H265 {
        let fmtp = if let (Some(vps_data), Some(sps_data), Some(pps_data)) = (vps, sps, pps) {
            let vps_b64 = base64::engine::general_purpose::STANDARD.encode(vps_data);
            let sps_b64 = base64::engine::general_purpose::STANDARD.encode(sps_data);
            let pps_b64 = base64::engine::general_purpose::STANDARD.encode(pps_data);

            if let Some(sps_info) = HevcSpsInfo::from_sps(sps_data) {
                Some(format!(
                    "sprop-vps={};sprop-sps={};sprop-pps={};profile-space={};profile-id={};tier-flag={};level-id={};interop-constraints={};tx-mode=0",
                    vps_b64, sps_b64, pps_b64,
                    sps_info.profile_space, sps_info.profile_idc,
                    sps_info.tier_flag, sps_info.level_idc,
                    sps_info.interop_constraints
                ))
            } else {
                Some(format!(
                    "sprop-vps={};sprop-sps={};sprop-pps={};profile-space=0;profile-id=1;tier-flag=0;level-id=93;interop-constraints=000000000000;tx-mode=0",
                    vps_b64, sps_b64, pps_b64
                ))
            }
        } else {
            Some("profile-space=0;profile-id=1;tier-flag=0;level-id=93".to_string())
        };

        builder.add_video_track(CodecType::H265, 98, 90000, &control_video, fmtp);
    } else {
        let fmtp = if let (Some(sps_data), Some(pps_data)) = (sps, pps) {
            let sps_b64 = base64::engine::general_purpose::STANDARD.encode(sps_data);
            let pps_b64 = base64::engine::general_purpose::STANDARD.encode(pps_data);
            let profile_level_id = if sps_data.len() >= 4 {
                format!("{:02X}{:02X}{:02X}", sps_data[1], sps_data[2], sps_data[3])
            } else {
                "42E01F".to_string()
            };
            Some(format!("packetization-mode=1;profile-level-id={};sprop-parameter-sets={},{}", profile_level_id, sps_b64, pps_b64))
        } else {
            Some("packetization-mode=1;profile-level-id=42E01F;sprop-parameter-sets=Z0LAHukBQBbsAAADAAQAAAMABAAAAwHNgYI=".to_string())
        };

        builder.add_video_track(CodecType::H264, 96, 90000, &control_video, fmtp);
    }

    let audio_fmtp = Some("profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3".to_string());
    builder.add_audio_track(CodecType::AAC, 97, 44100, 2, &control_audio, audio_fmtp);

    builder.build()
}

pub fn build_sdp_for_stream(
    stream_id: &str,
    tracks: &[Track],
    vps: Option<&[u8]>,
    sps: Option<&[u8]>,
    pps: Option<&[u8]>,
) -> String {
    let session_name = format!("MediaServer Session: {}", stream_id);
    let mut builder = RtspSdpBuilder::new(&session_name);

    for (idx, track) in tracks.iter().enumerate() {
        let control = format!("trackID={}", idx);

        match track.codec {
            CodecType::H264 => {
                let fmtp = if let (Some(sps_data), Some(pps_data)) = (sps, pps) {
                    let sps_b64 = base64::engine::general_purpose::STANDARD.encode(sps_data);
                    let pps_b64 = base64::engine::general_purpose::STANDARD.encode(pps_data);
                    let profile_level_id = if sps_data.len() >= 4 {
                        format!("{:02X}{:02X}{:02X}", sps_data[1], sps_data[2], sps_data[3])
                    } else {
                        "42E01F".to_string()
                    };
                    Some(format!("packetization-mode=1;profile-level-id={};sprop-parameter-sets={},{}", profile_level_id, sps_b64, pps_b64))
                } else {
                    Some("packetization-mode=1;profile-level-id=42E01F;sprop-parameter-sets=Z0LAHukBQBbsAAADAAQAAAMABAAAAwHNgYI=".to_string())
                };
                builder.add_video_track(track.codec, track.payload_type, track.clock_rate, &control, fmtp);
            }
            CodecType::H265 => {
                let fmtp = if let (Some(vps_data), Some(sps_data), Some(pps_data)) = (vps, sps, pps) {
                    let vps_b64 = base64::engine::general_purpose::STANDARD.encode(vps_data);
                    let sps_b64 = base64::engine::general_purpose::STANDARD.encode(sps_data);
                    let pps_b64 = base64::engine::general_purpose::STANDARD.encode(pps_data);

                    if let Some(sps_info) = HevcSpsInfo::from_sps(sps_data) {
                        Some(format!(
                            "sprop-vps={};sprop-sps={};sprop-pps={};profile-space={};profile-id={};tier-flag={};level-id={};interop-constraints={}",
                            vps_b64, sps_b64, pps_b64,
                            sps_info.profile_space, sps_info.profile_idc,
                            sps_info.tier_flag, sps_info.level_idc,
                            sps_info.interop_constraints
                        ))
                    } else {
                        Some(format!(
                            "sprop-vps={};sprop-sps={};sprop-pps={};profile-space=0;profile-id=1;tier-flag=0;level-id=93",
                            vps_b64, sps_b64, pps_b64
                        ))
                    }
                } else {
                    Some("profile-space=0;profile-id=1;tier-flag=0;level-id=93".to_string())
                };
                builder.add_video_track(track.codec, track.payload_type, track.clock_rate, &control, fmtp);
            }
            CodecType::AAC => {
                let fmtp = Some("profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;".to_string());
                builder.add_audio_track(track.codec, track.payload_type, track.clock_rate, 2, &control, fmtp);
            }
            CodecType::Opus => {
                builder.add_audio_track(track.codec, track.payload_type, track.clock_rate, 2, &control, None);
            }
            CodecType::G711 => {
                builder.add_audio_track(track.codec, track.payload_type, track.clock_rate, 1, &control, None);
            }
            _ => {
                builder.add_video_track(CodecType::H264, track.payload_type, 90000, &control, None);
            }
        }
    }

    builder.build()
}