use crate::manager::message::StreamTransmitEvent;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
// use serde_derive::Serialize;
use tokio::sync::mpsc;
use vcp_media_common::media::FrameData;

pub mod http_method_name {
    pub const OPTIONS: &str = "OPTIONS";
    pub const PATCH: &str = "PATCH";
    pub const POST: &str = "POST";
    pub const DELETE: &str = "DELETE";
    pub const GET: &str = "GET";
}
