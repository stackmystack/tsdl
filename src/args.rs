use std::{collections::BTreeMap, fmt, num::NonZeroUsize, path::PathBuf};

use clap::{
    builder::styling::{AnsiColor, Color, Style},
    crate_authors, ArgMatches,
};
use clap_verbosity_flag::{InfoLevel, Verbosity};
use serde::{Deserialize, Serialize};

use crate::consts::{
    TREE_SITTER_PLATFORM, TREE_SITTER_REPO, TREE_SITTER_VERSION, TSDL_BUILD_DIR, TSDL_CONFIG_FILE,
    TSDL_FORCE, TSDL_FRESH, TSDL_OUT_DIR, TSDL_PREFIX, TSDL_SHOW_CONFIG, TSDL_UNLOCK_TIMEOUT,
};

const TSDL_VERSION: &str = include_str!(concat!(env!("OUT_DIR"), "/tsdl.version"));

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
    #[arg(short, long, env = "TSDL_CONFIG", default_value = TSDL_CONFIG_FILE, global = true)]
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

#[derive(clap::ValueEnum, Clone, Debug, Deserialize, Serialize)]
pub enum LogColor {
    Auto,
    No,
    Yes,
}

#[derive(clap::ValueEnum, Clone, Debug, Deserialize, Serialize)]
pub enum ProgressStyle {
    Auto,
    Fancy,
    Plain,
}

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

    /// Update tsdl to the latest compatible version.
    #[command(visible_alias = "u")]
    Selfupdate {
        /// Skip the downgrade confirmation prompt.
        #[arg(long, default_value_t = false)]
        force: bool,

        /// Version selector: "patch", "minor", "major", or a semver like "2.5.0".
        #[arg(default_value = "minor")]
        target: String,
    },
}

impl Command {
    #[must_use]
    pub const fn is_build(&self) -> bool {
        matches!(self, Command::Build)
    }
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

impl fmt::Display for VersionBump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Major => write!(f, "major"),
            Self::Minor => write!(f, "minor"),
            Self::Patch => write!(f, "patch"),
        }
    }
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    #[default]
    Native,
    Wasm,
    All,
}

impl Target {
    #[must_use]
    pub fn covers(&self, other: Target) -> bool {
        matches!(
            (self, other),
            (Target::All, _) | (Target::Native, Target::Native) | (Target::Wasm, Target::Wasm)
        )
    }

    #[must_use]
    pub fn native(&self) -> bool {
        matches!(self, Self::All | Self::Native)
    }

    #[must_use]
    pub fn wasm(&self) -> bool {
        matches!(self, Self::All | Self::Wasm)
    }

    #[must_use]
    pub fn to_lowercase(&self) -> &'static str {
        match self {
            Target::Native => "native",
            Target::Wasm => "wasm",
            Target::All => "all",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
#[serde(rename_all = "kebab-case")]
pub enum ParserConfig {
    Full {
        #[serde(alias = "cmd", alias = "script")]
        build_script: Option<String>,

        from: Option<String>,

        #[serde(rename = "ref")]
        git_ref: String,
    },
    Ref(String),
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TreeSitter {
    #[serde(
        default = "default_tree_sitter_version",
        alias = "git-ref",
        alias = "ref"
    )]
    pub version: String,

    #[serde(default = "default_tree_sitter_platform")]
    pub platform: String,

    #[serde(default = "default_tree_sitter_repo")]
    pub repo: String,
}

impl Default for TreeSitter {
    fn default() -> Self {
        Self {
            version: TREE_SITTER_VERSION.to_string(),
            platform: TREE_SITTER_PLATFORM.to_string(),
            repo: TREE_SITTER_REPO.to_string(),
        }
    }
}

 // #[allow(clippy::struct_excessive_bools)]
 // #[derive(clap::Args, Clone, Debug, Deserialize, Diff, PartialEq, Eq, Serialize)]
 // #[diff(attr(
 //     #[derive(Debug, PartialEq)]
 // ))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct BuildCommand {
    pub build_dir: PathBuf,
    pub force: bool,
    pub fresh: bool,
    #[serde(skip_serializing)]
    pub languages: Option<Vec<String>>,
    pub jobs: NonZeroUsize,
    #[serde(rename = "out-dir", alias = "out")]
    pub out_dir: PathBuf,
    pub parsers: Option<BTreeMap<String, ParserConfig>>,
    pub prefix: String,
    pub show_config: bool,
    pub target: Target,
    pub tree_sitter: TreeSitter,
    pub unlock_timeout: u64,
}

impl Default for BuildCommand {
    fn default() -> Self {
        Self {
            build_dir: PathBuf::from(TSDL_BUILD_DIR),
            force: TSDL_FORCE,
            fresh: TSDL_FRESH,
            languages: None,
            jobs: default_jobs(),
            out_dir: PathBuf::from(TSDL_OUT_DIR),
            parsers: None,
            prefix: String::from(TSDL_PREFIX),
            show_config: TSDL_SHOW_CONFIG,
            target: Target::default(),
            tree_sitter: TreeSitter::default(),
            unlock_timeout: TSDL_UNLOCK_TIMEOUT,
        }
    }
}

#[must_use]
pub fn default_jobs() -> NonZeroUsize {
    NonZeroUsize::new(num_cpus::get()).unwrap_or(NonZeroUsize::MIN)
}

fn default_tree_sitter_version() -> String {
    TREE_SITTER_VERSION.to_string()
}

fn default_tree_sitter_platform() -> String {
    TREE_SITTER_PLATFORM.to_string()
}

fn default_tree_sitter_repo() -> String {
    TREE_SITTER_REPO.to_string()
}

#[derive(clap::Subcommand, Clone, Debug, Default)]
pub enum ConfigCommand {
    #[default]
    Current,
    Default,
}

impl fmt::Display for ConfigCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", format!("{self:?}").to_lowercase())
    }
}

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

#[must_use]
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
