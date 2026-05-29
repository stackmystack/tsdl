//! CLI argument definitions using clap.

use std::{collections::BTreeMap, fmt, num::NonZeroUsize, path::PathBuf};

use clap::{
    builder::styling::{AnsiColor, Color, Style},
    crate_authors, ArgMatches,
};
use clap_verbosity_flag::{InfoLevel, Verbosity};
use serde::{Deserialize, Serialize};

use crate::consts::{
    BUILD_DIR, CONFIG_FILE, FORCE, FRESH, PARSER_OUT_DIR, PLATFORM, PREFIX, REPO, SHOW_CONFIG,
    UNLOCK_TIMEOUT, VERSION,
};

// ============================================================
// Constants
// ============================================================

const TSDL_VERSION: &str = include_str!(concat!(env!("OUT_DIR"), "/tsdl.version"));

// ============================================================
// Enums
// ============================================================

/// CLI commands: build parsers, manage config, or self-update.
#[derive(clap::Subcommand, Clone, Debug)]
pub enum Command {
    /// Build one or many parsers.
    #[command(visible_alias = "b")]
    Build,

    /// Configuration helpers.
    #[command(visible_alias = "c")]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Update tsdl to its latest version.
    #[command(visible_alias = "u")]
    Selfupdate {
      #[arg(long, short, default_value="false")]
      force: bool,

      #[arg(long, short, default_value="minor")]
      target: String
    },
}

/// Subcommands for the `config` command.
#[derive(clap::Subcommand, Clone, Debug, Default)]
pub enum ConfigCommand {
    /// Show the currently active (merged) configuration.
    #[default]
    Current,
    /// Show the built-in default configuration.
    Default,
}

/// Whether to emit colored stderr logs (auto, on, off).
#[derive(clap::ValueEnum, Clone, Copy, Debug, Deserialize, Serialize)]
pub enum LogColor {
    /// Respect terminal capabilities.
    Auto,
    /// Force plain text (no ANSI escapes).
    No,
    /// Force colored output.
    Yes,
}

/// A single parser override, either a bare git ref string or a full block
/// with optional build script and custom repo URL.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
#[serde(rename_all = "kebab-case")]
pub enum ParserConfig {
    /// Full parser configuration block.
    Full {
        /// Custom build script (e.g. "make" or a shell command).
        #[serde(alias = "cmd", alias = "script")]
        build_script: Option<String>,

        /// Custom repository URL for this parser.
        from: Option<String>,

        /// Git ref to check out (tag, branch, or commit SHA).
        #[serde(rename = "ref")]
        git_ref: String,
    },
    /// Short form: a bare git ref string.
    Ref(String),
}

/// Terminal progress display style: auto-detect, force fancy (ratatui), or force plain.
#[derive(clap::ValueEnum, Clone, Debug, Deserialize, Serialize)]
pub enum ProgressStyle {
    /// Respect terminal TTY detection.
    Auto,
    /// Force ratatui inline rendering.
    Fancy,
    /// Force plain line-by-line output.
    Plain,
}

/// Build target: native shared library, WebAssembly, or both.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    /// Build a native `.so`/`.dylib`.
    #[default]
    Native,
    /// Build a `.wasm` binary.
    Wasm,
    /// Build both targets.
    All,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum VersionBump {
    /// Update to the latest version, including breaking changes.
    Major,
    /// Update to the latest minor or patch version (same MAJOR).
    #[default]
    Minor,
    /// Update to the latest patch version (same MAJOR.MINOR).
    Patch,
}

// ============================================================
// Structs
// ============================================================

/// Top-level CLI arguments.
#[derive(Clone, Debug, clap::Parser)]
#[command(author = crate_authors!("\n"), version = TSDL_VERSION, about, styles=get_styles(), allow_external_subcommands = true)]
#[command(help_template(
    "{before-help}{name} {version}
{author-with-newline}{about-with-newline}
{usage-heading} {usage}

{all-args}{after-help}"
))]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,

    /// Path to the config file (TOML).
    #[arg(short, long, env = "TSDL_CONFIG", default_value = CONFIG_FILE, global = true)]
    pub config: PathBuf,

    /// Path to the logging file. If unspecified, it will go to `build-dir/log`.
    /// If the path is inside `build-dir`, it must be directly under that directory.
    #[arg(short, long, env = "TSDL_LOG", global = true)]
    pub log: Option<PathBuf>,

    /// Whether to emit colored logs.
    #[arg(long, value_enum, default_value_t = LogColor::Auto, global = true)]
    pub log_color: LogColor,

    /// Progress style.
    #[arg(long, value_enum, default_value_t = ProgressStyle::Auto, global = true)]
    pub progress: ProgressStyle,

    /// Verbosity level: -v, -vv, or -q, -qq.
    #[command(flatten)]
    pub verbose: Verbosity<InfoLevel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct BuildCommand {
    /// Checkout and build directory root.
    pub build_dir: PathBuf,
    /// Ignore cache, force a full rebuild.
    pub force: bool,
    /// Clear build directory before starting.
    pub fresh: bool,
    /// Specific languages (positional CLI args); `None` means "all configured".
    #[serde(skip_serializing)]
    pub languages: Option<Vec<String>>,
    /// Max concurrent build jobs.
    pub jobs: NonZeroUsize,
    /// Directory for installed parser binaries.
    #[serde(rename = "out-dir", alias = "out")]
    pub out_dir: PathBuf,
    /// Per-parser configuration overrides from config file.
    pub parsers: Option<BTreeMap<String, ParserConfig>>,
    /// Output filename prefix for installed binaries.
    pub prefix: String,
    /// Print resolved config and exit.
    pub show_config: bool,
    /// Build output kind (native, wasm, or all).
    pub target: Target,
    /// Tree-sitter CLI version, platform, and repo.
    pub tree_sitter: TreeSitter,
    /// Seconds to wait for a stale lock to be released.
    pub unlock_timeout: u64,
}

/// Fully optional overrides for every build configuration field.
///
/// Every field is `Option<T>` so that config sources (TOML file,
/// environment variables, CLI flags) can be layered: missing keys
/// remain `None` while explicitly-set fields are `Some(...)`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OptionalBuildCommand {
    #[serde(default)]
    pub build_dir: Option<PathBuf>,
    #[serde(default)]
    pub force: Option<bool>,
    #[serde(default)]
    pub fresh: Option<bool>,
    #[serde(default, alias = "out", rename = "out-dir")]
    pub out_dir: Option<PathBuf>,
    #[serde(default)]
    pub jobs: Option<NonZeroUsize>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub show_config: Option<bool>,
    #[serde(default)]
    pub target: Option<Target>,
    #[serde(default)]
    pub tree_sitter: OptionalTreeSitter,
    #[serde(default)]
    pub unlock_timeout: Option<u64>,
    #[serde(default)]
    pub parsers: Option<BTreeMap<String, ParserConfig>>,
    #[serde(skip_deserializing)]
    pub languages: Option<Vec<String>>,
}

/// Fully optional overrides for [`TreeSitter`].
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OptionalTreeSitter {
    #[serde(default, alias = "git-ref", alias = "ref")]
    pub version: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
}

/// The tree-sitter CLI binary version, platform, and download repo.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TreeSitter {
    /// Git tag (or ref) of the tree-sitter CLI release.
    #[serde(
        default = "default_tree_sitter_version",
        alias = "git-ref",
        alias = "ref"
    )]
    pub version: String,

    /// Target platform string used to select the correct release asset.
    #[serde(default = "default_tree_sitter_platform")]
    pub platform: String,

    /// GitHub repository to download the CLI from (owner/name).
    #[serde(default = "default_tree_sitter_repo")]
    pub repo: String,
}

// ============================================================
// Free functions
// ============================================================

/// Get the build subcommand matches, if any.
#[must_use]
pub fn build_matches(matches: &ArgMatches) -> Option<&ArgMatches> {
    matches.subcommand().and_then(|(name, sub)| {
        if matches!(name, "build" | "b") {
            Some(sub)
        } else {
            None
        }
    })
}

/// Return the number of available CPUs as the default job count.
#[must_use]
pub fn default_jobs() -> NonZeroUsize {
    NonZeroUsize::new(num_cpus::get()).unwrap_or(NonZeroUsize::MIN)
}

/// Get the default tree-sitter platform from compile-time constants.
fn default_tree_sitter_platform() -> String {
    PLATFORM.to_string()
}

/// Get the default tree-sitter download repo from compile-time constants.
fn default_tree_sitter_repo() -> String {
    REPO.to_string()
}

/// Get the default tree-sitter version from compile-time constants.
fn default_tree_sitter_version() -> String {
    VERSION.to_string()
}

#[must_use]
/// Define the CLI colour scheme.
const fn get_styles() -> clap::builder::Styles {
    clap::builder::Styles::styled()
        .usage(
            Style::new()
                .bold()
                .fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
        )
        .header(
            Style::new()
                .bold()
                .fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
        )
        .literal(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Blue))))
        .invalid(
            Style::new()
                .bold()
                .fg_color(Some(Color::Ansi(AnsiColor::Red))),
        )
        .error(
            Style::new()
                .bold()
                .fg_color(Some(Color::Ansi(AnsiColor::Red))),
        )
        .valid(
            Style::new()
                .bold()
                .fg_color(Some(Color::Ansi(AnsiColor::Blue))),
        )
        .placeholder(Style::new().fg_color(Some(Color::Ansi(AnsiColor::White))))
}

// ============================================================
// Impls
// ============================================================

impl Command {
    /// Check whether this command is a build.
    #[must_use]
    pub const fn is_build(&self) -> bool {
        matches!(self, Command::Build)
    }
}

impl Default for BuildCommand {
    fn default() -> Self {
        Self {
            build_dir: PathBuf::from(BUILD_DIR),
            force: FORCE,
            fresh: FRESH,
            languages: None,
            jobs: default_jobs(),
            out_dir: PathBuf::from(PARSER_OUT_DIR),
            parsers: None,
            prefix: String::from(PREFIX),
            show_config: SHOW_CONFIG,
            target: Target::default(),
            tree_sitter: TreeSitter::default(),
            unlock_timeout: UNLOCK_TIMEOUT,
        }
    }
}

impl Default for TreeSitter {
    fn default() -> Self {
        Self {
            version: VERSION.to_string(),
            platform: PLATFORM.to_string(),
            repo: REPO.to_string(),
        }
    }
}

impl fmt::Display for ConfigCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", format!("{self:?}").to_lowercase())
    }
}

impl Target {
    /// Check whether `self` covers the requested target (e.g. All covers everything).
    #[must_use]
    pub fn covers(&self, other: Target) -> bool {
        matches!(
            (self, other),
            (Target::All, _) | (Target::Native, Target::Native) | (Target::Wasm, Target::Wasm)
        )
    }

    /// Combine two targets into the broadest coverage.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        match (self, other) {
            (Self::Native, Self::Native) => Self::Native,
            (Self::Wasm, Self::Wasm) => Self::Wasm,
            (Self::All, _)
            | (_, Self::All)
            | (Self::Native, Self::Wasm)
            | (Self::Wasm, Self::Native) => Self::All,
        }
    }

    /// Check whether the target includes native builds.
    #[must_use]
    pub fn native(&self) -> bool {
        matches!(self, Self::All | Self::Native)
    }

    /// Check whether the target includes wasm builds.
    #[must_use]
    pub fn wasm(&self) -> bool {
        matches!(self, Self::All | Self::Wasm)
    }

    /// Return a lowercased string representation.
    #[must_use]
    pub fn to_lowercase(&self) -> &'static str {
        match self {
            Target::Native => "native",
            Target::Wasm => "wasm",
            Target::All => "all",
        }
    }
}

impl fmt::Display for VersionBump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Major => write!(f, "major"),
            Self::Minor => write!(f, "minor"),
            Self::Patch => write!(f, "patch"),
        }
    }
}
