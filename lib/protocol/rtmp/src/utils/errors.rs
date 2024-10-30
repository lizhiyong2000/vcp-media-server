use {
    thiserror::Error,
    std::fmt,
};


#[derive(Debug, Error)]
pub enum RtmpUrlParseError {
    #[error("The url is not valid")]
    Notvalid,
}

