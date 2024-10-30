use thiserror::Error;
use std::fmt;



#[derive(Debug, Error)]
pub enum AuthError {
    #[error("token is not correct.")]
    TokenIsNotCorrect,
    #[error("no token found.")]
    NoTokenFound,
}

