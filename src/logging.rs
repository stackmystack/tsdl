use std::{
    ffi::OsStr,
    fs::{self, File},
    path::{Path, PathBuf},
};

use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_log::AsTrace;
use tracing_subscriber::{layer::SubscriberExt, Layer};

use crate::{absolute_normalize, args, consts, Error, Result, ResultExt};

// ============================================================
// Enums
// ============================================================

pub enum Implicit<'a> {
    BuildDir { dir: &'a Path },
    None,
}

// ============================================================
// Structs
// ============================================================

#[allow(dead_code)]
pub struct Guard(WorkerGuard);

pub struct Policy<'a> {
    pub explicit: Option<&'a Path>,
    pub implicit: Implicit<'a>,
}

pub struct Session {
    path: Option<PathBuf>,
    _guard: Option<Guard>,
}

// ============================================================
// Free functions
// ============================================================

fn file_layer(
    writer: tracing_appender::non_blocking::NonBlocking,
    color: bool,
    filter: LevelFilter,
) -> Box<dyn Layer<tracing_subscriber::Registry> + Send + Sync> {
    tracing_subscriber::fmt::layer()
        .compact()
        .with_ansi(color)
        .with_file(true)
        .with_level(true)
        .with_line_number(true)
        .with_target(true)
        .with_thread_ids(true)
        .with_writer(writer)
        .with_filter(filter)
        .boxed()
}

pub fn init(
    policy: Policy<'_>,
    log_color: args::LogColor,
    verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,
) -> Result<Session> {
    let color = match log_color {
        args::LogColor::Auto => atty::is(atty::Stream::Stdout),
        args::LogColor::No => false,
        args::LogColor::Yes => true,
    };
    console::set_colors_enabled(color);

    let filter = verbose.log_level_filter().as_trace();
    let path = resolve_log_path(policy)?;
    let (writer, guard) = match path.as_ref() {
        Some(path) => {
            let file = open_log_file(path)?;
            let (writer, guard) = tracing_appender::non_blocking(file);
            (Some(writer), Some(Guard(guard)))
        }
        None => (None, None),
    };

    init_tracing(writer, color, filter);
    Ok(Session {
        path,
        _guard: guard,
    })
}

fn init_tracing(
    writer: Option<tracing_appender::non_blocking::NonBlocking>,
    color: bool,
    filter: LevelFilter,
) {
    let mut layers: Vec<Box<dyn Layer<tracing_subscriber::Registry> + Send + Sync>> = Vec::new();

    if let Some(writer) = writer {
        layers.push(file_layer(writer, color, filter));
    }

    if filter == LevelFilter::DEBUG || filter == LevelFilter::TRACE {
        layers.push(stderr_layer(color, filter));
    }

    let subscriber = tracing_subscriber::registry().with(layers);
    tracing::subscriber::set_global_default(subscriber).unwrap();
}

fn open_log_file(log: &Path) -> Result<File> {
    let parent = log.parent().unwrap_or(Path::new("."));
    if !parent.exists() {
        fs::create_dir_all(parent).context("Preparing log directory")?;
    }
    File::create(log).context("Creating log file")
}

fn resolve_log_path(policy: Policy<'_>) -> Result<Option<PathBuf>> {
    match (policy.explicit, policy.implicit) {
        (Some(log), Implicit::BuildDir { dir }) => validate_log_path(dir, log).map(Some),
        (Some(log), Implicit::None) => validate_standalone_log_path(log).map(Some),
        (None, Implicit::BuildDir { dir }) => {
            validate_log_path(dir, &dir.join(consts::LOG_FILE)).map(Some)
        }
        (None, Implicit::None) => Ok(None),
    }
}

fn stderr_layer(
    color: bool,
    filter: LevelFilter,
) -> Box<dyn Layer<tracing_subscriber::Registry> + Send + Sync> {
    tracing_subscriber::fmt::layer()
        .compact()
        .with_ansi(color)
        .with_file(true)
        .with_level(true)
        .with_line_number(true)
        .with_target(true)
        .with_thread_ids(false)
        .with_writer(std::io::stderr)
        .without_time()
        .with_filter(filter)
        .boxed()
}

fn validate_log_path(build_dir: &Path, log: &Path) -> Result<PathBuf> {
    let build_dir = absolute_normalize(build_dir)?;
    let log = absolute_normalize(log)?;

    if log == build_dir {
        return Err(Error::Message {
            message: format!(
                "--log must be a file path, not the build directory {}",
                build_dir.display()
            ),
        });
    }

    if log.is_dir() {
        return Err(Error::Message {
            message: format!(
                "--log must be a file path, not a directory: {}",
                log.display()
            ),
        });
    }

    if log.starts_with(&build_dir) {
        let relative = log.strip_prefix(&build_dir).map_err(|e| Error::Message {
            message: format!(
                "Could not compare log path {} with build directory {}: {e}",
                log.display(),
                build_dir.display()
            ),
        })?;
        let component_count = relative.components().count();

        if component_count != 1 {
            return Err(Error::Message {
                message: format!(
                    "--log path {} is nested inside --build-dir {}. Put logs directly under the build directory root or outside it.",
                    log.display(),
                    build_dir.display()
                ),
            });
        }

        let Some(name) = relative.file_name() else {
            return Err(Error::Message {
                message: format!("--log must be a file path: {}", log.display()),
            });
        };

        if name == OsStr::new(consts::LOCK_FILE) || name == OsStr::new(consts::CACHE_FILE) {
            return Err(Error::Message {
                message: format!(
                    "--log path {} conflicts with a tsdl runtime/build file",
                    log.display()
                ),
            });
        }
    }

    Ok(log)
}

fn validate_standalone_log_path(log: &Path) -> Result<PathBuf> {
    let log = absolute_normalize(log)?;

    if log.is_dir() {
        return Err(Error::Message {
            message: format!(
                "--log must be a file path, not a directory: {}",
                log.display()
            ),
        });
    }

    Ok(log)
}

// ============================================================
// Impls
// ============================================================

impl Session {
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}
