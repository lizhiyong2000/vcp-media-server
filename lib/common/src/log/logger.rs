use std::str::FromStr;

use chrono::Local;
use tracing_appender::non_blocking::WorkerGuard;
// use tracing::{info, instrument};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{self, filter::LevelFilter, fmt, fmt::format::Writer, fmt::time::FormatTime};

// use tracing_subscriber::{fmt::Layer, prelude::*, EnvFilter, Registry};

use tracing_appender;

// 用来格式化日志的输出时间格式
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
struct LocalTimer;

impl FormatTime for LocalTimer {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", Local::now().format("%FT%T%.3f"))
    }
}

pub fn setup_log() -> WorkerGuard {
    
    let format = tracing_subscriber::fmt::format()
        .with_level(true)
        .with_target(true)
        // .with_file(true)
        .with_line_number(true)
        .with_timer(LocalTimer).compact();

    let file_appender = tracing_appender::rolling::daily("./", "mediaserver.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    // 初始化并设置日志格式(定制和筛选日志)
    tracing_subscriber::registry()
        .with(LevelFilter::from_str("INFO").unwrap())
        .with(
            fmt::Layer::new()
                .event_format(format.clone())
                .with_writer(non_blocking)
                .with_ansi(false),
        )
        .with(
            fmt::Layer::new()
                .event_format(format.clone())
                .with_writer(std::io::stdout),
        )
        .init(); // 初始化并将SubScriber设置为全局SubScriber

    return _guard;
}
