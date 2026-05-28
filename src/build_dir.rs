use std::fmt;
use std::path::{Path, PathBuf};

use crate::{absolute_normalize, consts, Result};

/// The root build directory — anchor for all derived paths.
///
/// Wrapping this in a newtype prevents accidental swaps with other `PathBuf`
/// values (e.g. `out_dir`) and centralises sub-path derivation.
#[derive(Clone, Debug)]
pub struct BuildDir(PathBuf);

impl BuildDir {
    /// Create a `BuildDir`, resolving relative paths against the current
    /// working directory and normalising `.` / `..` components.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = absolute_normalize(&path.into())?;
        Ok(Self(path))
    }

    /// The absolute, normalised path as a `&Path`.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Path to the build cache TOML file inside this build directory.
    #[must_use]
    pub fn cache_file(&self) -> PathBuf {
        self.0.join(consts::CACHE_FILE)
    }

    /// Path to the PID lock file inside this build directory.
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.0.join(consts::LOCK_FILE)
    }

    /// Path to the default log file inside this build directory.
    #[must_use]
    pub fn log_file(&self) -> PathBuf {
        self.0.join(consts::LOG_FILE)
    }

    /// Per-language checkout directory: `<build_dir>/tree-sitter-<language>`.
    #[must_use]
    pub fn checkout_dir(&self, language: &str) -> PathBuf {
        self.0.join(format!("tree-sitter-{language}"))
    }
}

impl AsRef<Path> for BuildDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for BuildDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}
