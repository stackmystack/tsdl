use std::{
    ffi::OsStr,
    fs::{self, File},
    path::{Path, PathBuf},
};

use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_log::AsTrace;
use tracing_subscriber::{layer::SubscriberExt, Layer};

use crate::{
    absolute_normalize,
    args::{Args, LogColor},
    config::current,
    consts::{TSDL_BUILD_DIR, TSDL_CACHE_FILE, TSDL_LOCK_FILE, TSDL_LOG_FILE},
    error::TsdlError,
    TsdlResult,
};

pub struct Logging {
    pub path: PathBuf,
    _guard: WorkerGuard,
}

pub fn init(args: &Args) -> TsdlResult<Logging> {
    let color = match args.log_color {
        LogColor::Auto => atty::is(atty::Stream::Stdout),
        LogColor::No => false,
        LogColor::Yes => true,
    };
    console::set_colors_enabled(color);
    let filter = args.verbose.log_level_filter().as_trace();
    let path = resolve_log_path(args)?;
    let file = open_log_file(&path)?;
    let guard = init_tracing(file, color, filter);
    Ok(Logging {
        path,
        _guard: guard,
    })
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

fn resolve_log_path(args: &Args) -> TsdlResult<PathBuf> {
    let command = current(&args.config, args.command.as_build()).ok();
    let build_dir = command
        .as_ref()
        .map_or_else(|| PathBuf::from(TSDL_BUILD_DIR), |c| c.build_dir.clone());
    let log = args
        .log
        .as_ref()
        .map_or_else(|| build_dir.join(TSDL_LOG_FILE), std::clone::Clone::clone);

    validate_log_path(&build_dir, &log)
}

fn validate_log_path(build_dir: &Path, log: &Path) -> TsdlResult<PathBuf> {
    let build_dir = absolute_normalize(build_dir)?;
    let log = absolute_normalize(log)?;

    if log == build_dir {
        return Err(TsdlError::message(format!(
            "--log must be a file path, not the build directory {}",
            build_dir.display()
        )));
    }

    if log.is_dir() {
        return Err(TsdlError::message(format!(
            "--log must be a file path, not a directory: {}",
            log.display()
        )));
    }

    if log.starts_with(&build_dir) {
        let relative = log.strip_prefix(&build_dir).map_err(|e| {
            TsdlError::message(format!(
                "Could not compare log path {} with build directory {}: {e}",
                log.display(),
                build_dir.display()
            ))
        })?;
        let component_count = relative.components().count();

        if component_count != 1 {
            return Err(TsdlError::message(format!(
                "--log path {} is nested inside --build-dir {}. Put logs directly under the build directory root or outside it.",
                log.display(),
                build_dir.display()
            )));
        }

        let Some(name) = relative.file_name() else {
            return Err(TsdlError::message(format!(
                "--log must be a file path: {}",
                log.display()
            )));
        };

        if name == OsStr::new(TSDL_LOCK_FILE) || name == OsStr::new(TSDL_CACHE_FILE) {
            return Err(TsdlError::message(format!(
                "--log path {} conflicts with a tsdl runtime/build file",
                log.display()
            )));
        }
    }

    Ok(log)
}

fn open_log_file(log: &Path) -> TsdlResult<File> {
    let parent = log.parent().unwrap_or(Path::new("."));
    if !parent.exists() {
        fs::create_dir_all(parent).map_err(|e| TsdlError::context("Preparing log directory", e))?;
    }
    File::create(log).map_err(|e| TsdlError::context("Creating log file", e))
}
