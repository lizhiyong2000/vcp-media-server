use thiserror::Error;


#[derive(Debug, Error)]
pub enum RtmpUrlParseError {
    #[error("The url is not valid")]
    Notvalid,
}

