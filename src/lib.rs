//! A downloader/builder of many [tree-sitter](https://github.com/tree-sitter/tree-sitter) parsers
//!
//! # Why?
//!
//! To build parsers (`.so`/`.dylib`) and use them with your favourite bindings.
//!
//! I created it more specifically for the [ruby bindings](https://github.com/Faveod/ruby-tree-sitter).
//!
//! ## Configuration
//!
//! If no configuration is provided for the language you're asking for in `parsers.toml`,
//! the latest parsers will be downloaded built.
//!
//! If you wish to pin parser versions:
//!
//! ```toml
//! [parsers]
//! java = "v0.21.0"
//! json = "0.21.0" # The leading v is not necessary
//! python = "master"
//! typescript = { ref = "0.21.0", cmd = "make" }
//! cobol = { ref = "6a469068cacb5e3955bb16ad8dfff0dd792883c9", from = "https://github.com/yutaro-sakamoto/tree-sitter-cobol" }
//! ```
//!
//! Run:
//!
//! ```sh
//! tsdl config default
//! ```
//!
//! to get the default config used by tsdl in TOML.
//!
//! All configuration you can pass to `tsd build` can be put in the `parsers.toml`,
//! like `tree-sitter-ref`, `out-dir`, etc.
//!
//! ```toml
//! build-dir = "/tmp/tsdl"
//! out-dir = "/usr/local/lib"
//!
//! [parsers]
//! json = "0.21.0" # The leading v is not necessary
//! rust = "master"
//! ```
//!
//! All configuration specified in `parsers.toml` can be overridden with flags
//! passed to `tsdl`, i.e.: `tsdl build --build-dir "/tmp/tsdl"` will
//! override whatever value is the default of `tsdl` or in `parsers.toml`.
//!
//! Check out [Faveod/tree-sitter-parsers](https://github.com/Faveod/tree-sitter-parsers) for an
//! example configuration.

use std::{
    env,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use crate::error::TsdlError;

extern crate log;

pub mod actors;
pub mod app;
pub mod args;
pub mod build;
pub mod cache;
pub mod config;
pub mod consts;
pub mod display;
pub mod error;
pub mod git;
pub mod lock;
pub mod logging;
pub mod parser;
#[macro_use]
pub mod sh;
pub mod tree_sitter;
pub mod walk;

pub trait SafeCanonicalize {
    fn canon(&self) -> TsdlResult<PathBuf>;
}

impl SafeCanonicalize for Path {
    fn canon(&self) -> TsdlResult<PathBuf> {
        if self.is_absolute() {
            Ok(self.to_path_buf())
        } else {
            let current_dir = env::current_dir()
                .map_err(|e| TsdlError::context("Failed to get current directory", e))?;
            Ok(current_dir.join(self))
        }
    }
}

impl SafeCanonicalize for PathBuf {
    fn canon(&self) -> TsdlResult<PathBuf> {
        self.as_path().canon()
    }
}

#[must_use]
pub fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let millis = duration.subsec_millis();

    // Base case: sub-minute gets full precision
    if total_seconds < 60 {
        return format!("{total_seconds}.{millis:02}s");
    }

    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = total_seconds / 3600;

    let mut parts = Vec::new();

    if hours > 0 {
        parts.push(format!("{hours}h"));
    }

    if minutes > 0 {
        parts.push(format!("{minutes}mn"));
    }

    if seconds > 0 || millis > 0 {
        if millis > 0 {
            parts.push(format!("{seconds}.{millis:03}s"));
        } else {
            parts.push(format!("{seconds}s"));
        }
    }

    parts.join(" ")
}

pub fn relative_to_cwd(dir: &Path) -> PathBuf {
    let canon = dir.canon().unwrap_or_else(|_| dir.to_path_buf());
    let cwd = env::current_dir().unwrap_or_else(|_| dir.to_path_buf());

    if canon != cwd && canon.starts_with(&cwd) {
        dir.strip_prefix(cwd).map_or(canon, Path::to_path_buf)
    } else {
        canon
    }
}

/// Result type for tsdl operations
pub type TsdlResult<T> = Result<T, error::TsdlError>;

/// Prompt user for confirmation with default behavior
pub fn prompt_user(question: &str, default_yes: bool) -> TsdlResult<bool> {
    let options = if default_yes { "[Y/n]" } else { "[y/N]" };

    eprint!("{question} {options}: ");

    let _ = io::stderr().flush();
    let mut input = String::new();

    io::stdin()
        .read_line(&mut input)
        .map_err(|e| TsdlError::context("Reading user input", e))?;

    let input = input.trim().to_lowercase();

    if input.is_empty() {
        return Ok(default_yes);
    }

    Ok(input == "y")
}
