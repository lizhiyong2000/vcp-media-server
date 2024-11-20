use thiserror::Error;


#[derive(Debug, Error)]
pub enum AuthError {
    #[error("token is not correct.")]
    TokenIsNotCorrect,
    #[error("no token found.")]
    NoTokenFound,
}

