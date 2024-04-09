use std::error;
use tokio::signal;

mod utils;
use utils::logger;

type Result<T> = std::result::Result<T, Box<dyn error::Error>>;

use log::{self, info};

#[tokio::main]
async fn main() -> Result<()> {
    logger::setup_log();

    info!("setup main");

    signal::ctrl_c().await?;
    Ok(())
}
