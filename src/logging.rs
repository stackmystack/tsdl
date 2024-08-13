use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use miette::{Context, IntoDiagnostic as _, Result};
use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
#[cfg(debug_assertions)]
use tracing_error::ErrorLayer;
use tracing_log::AsTrace;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    args::{Args, LogColor},
    config::current,
    consts::TSDL_BUILD_DIR,
    SafeCanonicalize,
};

pub fn init(args: &Args) -> Result<WorkerGuard> {
    let color = match args.log_color {
        LogColor::Auto => atty::is(atty::Stream::Stdout),
        LogColor::No => false,
        LogColor::Yes => true,
    };
    console::set_colors_enabled(color);
    let filter = args.verbose.log_level_filter().as_trace();
    let without_time = std::env::var("TSDL_LOG_TIME")
        .map(|v| !matches!(v.to_lowercase().as_str(), "1" | "y" | "yes"))
        .unwrap_or(true);
    let file = init_log_file(args)?;
    Ok(init_tracing(file, color, filter, without_time))
}

fn init_tracing(file: File, color: bool, filter: LevelFilter, without_time: bool) -> WorkerGuard {
    let (writer, guard) = tracing_appender::non_blocking(file);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .compact()
        .with_ansi(color)
        .with_file(true)
        .with_level(true)
        .with_line_number(true)
        .with_target(true)
        .with_thread_ids(true)
        .with_writer(writer);
    if without_time {
        let fmt_layer = fmt_layer.without_time();
        let registry = tracing_subscriber::registry().with(fmt_layer).with(filter);
        #[cfg(debug_assertions)]
        {
            registry.with(ErrorLayer::default()).init();
        }
        #[cfg(not(debug_assertions))]
        {
            registry.init();
        }
    } else {
        let registry = tracing_subscriber::registry().with(fmt_layer).with(filter);
        #[cfg(debug_assertions)]
        {
            registry.with(ErrorLayer::default()).init();
        }
        #[cfg(not(debug_assertions))]
        {
            registry.init();
        }
    };
    guard
}

fn init_log_file(args: &Args) -> Result<File> {
    let log = args
        .log
        .as_ref()
        .filter(|l| {
            l.canon()
                .ok()
                .and_then(|p| p.parent().map(Path::exists))
                .unwrap_or_default()
        })
        .cloned()
        .or_else(|| {
            current(&args.config, args.command.as_build())
                .map(|c| Some(c.build_dir.clone().join("log")))
                .unwrap_or_default()
        })
        .unwrap_or(PathBuf::from(TSDL_BUILD_DIR).join("log"));
    let parent = log.parent().unwrap_or(Path::new("."));
    if !parent.exists() {
        fs::create_dir_all(parent)
            .into_diagnostic()
            .wrap_err("Preparing log directory")?;
    }
    File::create(&log)
        .into_diagnostic()
        .wrap_err("Creating log file")
}
