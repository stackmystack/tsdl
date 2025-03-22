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
//! like `tree-sitter-version`, `out-dir`, etc.
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
    path::{Path, PathBuf},
    time,
};

use miette::{IntoDiagnostic, Result};

extern crate log;

pub mod args;
pub mod build;
pub mod config;
pub mod consts;
pub mod display;
pub mod error;
pub mod git;
pub mod logging;
pub mod parser;
#[macro_use]
pub mod sh;
pub mod tree_sitter;

pub trait SafeCanonicalize {
    fn canon(&self) -> Result<PathBuf>;
}

impl SafeCanonicalize for Path {
    fn canon(&self) -> Result<PathBuf> {
        if self.is_absolute() {
            Ok(self.to_path_buf())
        } else {
            let current_dir = env::current_dir().into_diagnostic()?;
            Ok(current_dir.join(self))
        }
    }
}

impl SafeCanonicalize for PathBuf {
    fn canon(&self) -> Result<PathBuf> {
        self.as_path().canon()
    }
}
fn format_duration(duration: time::Duration) -> String {
    let total_seconds = duration.as_secs();
    let milliseconds = duration.subsec_millis();
    if total_seconds < 60 {
        format!("{total_seconds}.{milliseconds:#02}s")
    } else {
        format!("{}mn {}s", total_seconds % 60, total_seconds / 60)
    }
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
