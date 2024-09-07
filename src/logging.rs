use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use miette::{Context, IntoDiagnostic as _, Result};
use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_log::AsTrace;
use tracing_subscriber::{layer::SubscriberExt, Layer};

use crate::{
    args::{Args, LogColor},
    config::current,
    consts::TSDL_BUILD_DIR,
};

pub fn init(args: &Args) -> Result<WorkerGuard> {
    let color = match args.log_color {
        LogColor::Auto => atty::is(atty::Stream::Stdout),
        LogColor::No => false,
        LogColor::Yes => true,
    };
    console::set_colors_enabled(color);
    let filter = args.verbose.log_level_filter().as_trace();
    let file = init_log_file(args)?;
    Ok(init_tracing(file, color, filter))
}

fn init_tracing(file: File, color: bool, filter: LevelFilter) -> WorkerGuard {
    let (writer, guard) = tracing_appender::non_blocking(file);
    let stdout_layer = tracing_subscriber::fmt::layer()
        .compact()
        .with_ansi(color)
        .with_file(true)
        .with_level(true)
        .with_line_number(true)
        .with_target(true)
        .with_thread_ids(false)
        .with_writer(std::io::stderr)
        .without_time()
        .with_filter(filter);
    let file_layer = tracing_subscriber::fmt::layer()
        .compact()
        .with_ansi(color)
        .with_file(true)
        .with_level(true)
        .with_line_number(true)
        .with_target(true)
        .with_thread_ids(true)
        .with_writer(writer)
        .with_filter(filter);
    if filter == LevelFilter::DEBUG || filter == LevelFilter::TRACE {
        let subscriber = tracing_subscriber::registry()
            .with(file_layer)
            .with(stdout_layer);
        tracing::subscriber::set_global_default(subscriber).unwrap();
    } else {
        let subscriber = tracing_subscriber::registry().with(file_layer);
        tracing::subscriber::set_global_default(subscriber).unwrap();
    }
    guard
}

fn init_log_file(args: &Args) -> Result<File> {
    let log = args.log.as_ref().map_or_else(
        || {
            current(&args.config, args.command.as_build()).map_or_else(
                |_| PathBuf::from(TSDL_BUILD_DIR).join("log"),
                |c| c.build_dir.clone().join("log"),
            )
        },
        std::clone::Clone::clone,
    );
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
