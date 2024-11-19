use std::fmt::{write, Debug, Display, Formatter};
use bytes::BytesMut;

#[derive(Clone, PartialEq, Debug)]
pub enum VideoCodecType {
    H264,
    H265,
}

#[derive(Clone, PartialEq, Debug)]
pub enum AudioCodecType {
    G711,
    AC3,
}



#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub audio_clock_rate: u32,
    pub video_clock_rate: u32,
    pub video_codec: VideoCodecType,
    pub audio_codec: AudioCodecType,
}

impl Display for MediaInfo {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "MediaInfo[video_codec:{:?} audio_clock_rate:{:?} video_clock_rate:{:?}  audio_codec:{:?}]",
               self.video_codec, self.video_clock_rate, self.audio_codec,
               self.audio_clock_rate)
    }
}

#[derive(Clone)]
pub enum FrameData {
    Video { timestamp: u32, data: BytesMut },
    Audio { timestamp: u32, data: BytesMut },
    MetaData { timestamp: u32, data: BytesMut },
    MediaInfo { media_info: MediaInfo },
}

impl Debug for FrameData {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {

        match self {
            FrameData::Video { timestamp, data } => {
                write!(f, "Video FrameData, timestamp: {}, data.len: {}", timestamp, data.len())
            }
            FrameData::Audio { timestamp, data } => {
                write!(f, "Audio FrameData, timestamp: {}, data.len: {}", timestamp, data.len())
            }
            FrameData::MetaData { timestamp, data } => {
                write!(f, "MetaData FrameData, timestamp: {}, data.len: {}", timestamp, data.len())
            }
            FrameData::MediaInfo { media_info } => {
                write!(f, "MediaInfo FrameData, media_info: {}", media_info)
            }
        }
    }
}


//Used to pass rtp raw data.
#[derive(Clone)]
pub enum PacketData {
    Video { timestamp: u32, data: BytesMut },
    Audio { timestamp: u32, data: BytesMut },
}

//used to save data which needs to be transferred between client/server sessions
#[derive(Clone)]
pub enum Information {
    Sdp { data: String },
}