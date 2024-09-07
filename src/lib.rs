#![doc = include_str!("../README.md")]
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
