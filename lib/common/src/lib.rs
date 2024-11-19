pub mod bytesio;
pub mod log;
pub mod http;
pub mod macros;
pub mod uuid;
pub mod server;
pub mod media;

use std::error;

pub type Result<T> = std::result::Result<T, Box<dyn error::Error>>;

pub trait Unmarshal<T1, T2> {
    fn unmarshal(data: T1) -> T2
    where
        Self: Sized;
}

pub trait Marshal<T> {
    fn marshal(&self) -> T;
}
