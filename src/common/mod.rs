pub mod marshal_trait;
pub mod http;
pub mod errors;
pub mod auth;

pub mod macros;
pub mod uuid;

use std::error;

pub type Result<T> = std::result::Result<T, Box<dyn error::Error>>;
