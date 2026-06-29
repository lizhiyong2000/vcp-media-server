use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClientSignal {
    Publish {
        stream_id: String,
        sdp: String,
    },
    Play {
        stream_id: String,
        sdp: String,
    },
    Ice {
        candidate: String,
        #[serde(default)]
        sdp_mid: Option<String>,
        #[serde(default)]
        sdp_mline_index: Option<u16>,
    },
  /// Client stopped publishing; release peer connection and stream state.
  #[serde(rename = "stop_publish")]
  StopPublish {
    stream_id: String,
  },
  /// Client stopped playback; release relay and peer connection.
  #[serde(rename = "stop_play")]
  StopPlay {
    stream_id: String,
  },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ServerSignal {
    Answer {
        sdp: String,
    },
    Ice {
        candidate: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sdp_mid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sdp_mline_index: Option<u16>,
    },
    Error {
        message: String,
    },
    /// Ask the publisher browser to emit an H264 IDR (for play catch-up).
    #[serde(rename = "need_keyframe")]
    NeedKeyframe,
}

impl ServerSignal {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"type":"error","message":"serialization failed"}"#.to_string()
        })
    }
}
