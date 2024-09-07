use std::{collections::BTreeMap, fmt, path::PathBuf};

use clap::{
    builder::styling::{AnsiColor, Color, Style},
    crate_authors,
};
use clap_verbosity_flag::{InfoLevel, Verbosity};
use diff::Diff;
use serde::{Deserialize, Serialize};

use crate::consts::{
    TREE_SITTER_PLATFORM, TREE_SITTER_REPO, TREE_SITTER_VERSION, TSDL_BUILD_DIR, TSDL_CONFIG_FILE,
    TSDL_FRESH, TSDL_OUT_DIR, TSDL_PREFIX, TSDL_SHOW_CONFIG,
};

const TSDL_VERSION: &str = include_str!(concat!(env!("OUT_DIR"), "/tsdl.version"));

/// Command-line arguments.
#[derive(Clone, Debug, Deserialize, clap::Parser, Serialize)]
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
    #[arg(short, long, default_value = TSDL_CONFIG_FILE, global = true)]
    pub config: PathBuf,

    /// Path to the logging file. If unspecified, it will go to `build-dir/log`.
    #[arg(short, long, global = true)]
    pub log: Option<PathBuf>,

    /// Whether to emit colored logs.
    #[arg(long, value_enum, default_value_t = LogColor::Auto, global = true)]
    pub log_color: LogColor,

    /// Progress style.
    #[arg(long, value_enum, default_value_t = ProgressStyle::Auto, global = true)]
    pub progress: ProgressStyle,

    /// Verbosity level: -v, -vv, or -q, -qq.
    // clap_verbosity_flag, as of now, refuses to add a serialization feature, so this will not be part of the config file.
    // It's global by default, so we don't need to specify it.
    #[serde(skip_serializing, skip_deserializing)]
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

#[derive(clap::Subcommand, Clone, Debug, Deserialize, Serialize)]
pub enum Command {
    /// Build one or many parsers.
    #[command(visible_alias = "b")]
    Build(BuildCommand),

    /// Configuration helpers.
    #[serde(skip_serializing, skip_deserializing)]
    #[command(visible_alias = "c")]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Update tsdl to its latest version.
    #[serde(skip_serializing, skip_deserializing)]
    #[command(visible_alias = "u")]
    Selfupdate,
}

impl Command {
    #[must_use]
    pub fn as_build(&self) -> Option<&BuildCommand> {
        if let Command::Build(build) = self {
            Some(build)
        } else {
            None
        }
    }

    #[must_use]
    pub fn as_config(&self) -> Option<&ConfigCommand> {
        if let Command::Config { command } = self {
            Some(command)
        } else {
            None
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(clap::Args, Clone, Debug, Deserialize, Diff, PartialEq, Eq, Serialize)]
#[diff(attr(
    #[derive(Debug, PartialEq)]
))]
#[serde(rename_all = "kebab-case")]
pub struct BuildCommand {
    /// Parsers to compile.
    #[serde(skip_serializing, skip_deserializing)]
    #[arg(verbatim_doc_comment)]
    pub languages: Option<Vec<String>>,

    /// Configured Parsers.
    #[clap(skip)]
    pub parsers: Option<BTreeMap<String, ParserConfig>>,

    /// Build Directory.
    #[serde(default)]
    #[arg(short, long, default_value = TSDL_BUILD_DIR)]
    pub build_dir: PathBuf,

    /// Number of threads; defaults to the number of available CPUs.
    #[arg(short, long, default_value_t = num_cpus::get())]
    #[serde(default)]
    pub ncpus: usize,

    /// Clears the `build-dir` and starts a fresh build.
    #[arg(short, long, default_value_t = TSDL_FRESH)]
    #[serde(default)]
    pub fresh: bool,

    /// Output Directory.
    #[arg(short, long, default_value = TSDL_OUT_DIR)]
    #[serde(default)]
    pub out_dir: PathBuf,

    /// Prefix parser names.
    #[arg(short, long, default_value = TSDL_PREFIX)]
    #[serde(default)]
    pub prefix: String,

    /// Show Config.
    #[arg(long, default_value_t = TSDL_SHOW_CONFIG)]
    #[serde(default)]
    pub show_config: bool,

    #[command(flatten)]
    #[serde(default)]
    pub tree_sitter: TreeSitter,
}

impl Default for BuildCommand {
    fn default() -> Self {
        Self {
            languages: None,
            parsers: None,
            build_dir: PathBuf::from(TSDL_BUILD_DIR),
            fresh: TSDL_FRESH,
            ncpus: num_cpus::get(),
            out_dir: PathBuf::from(TSDL_OUT_DIR),
            prefix: String::from(TSDL_PREFIX),
            show_config: TSDL_SHOW_CONFIG,
            tree_sitter: TreeSitter::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Diff, Serialize, PartialEq, Eq)]
#[diff(attr(
    #[derive(Debug, PartialEq)]
))]
#[serde(untagged)]
#[serde(rename_all = "kebab-case")]
pub enum ParserConfig {
    Full {
        #[serde(alias = "cmd", alias = "script")]
        build_script: Option<String>,
        #[serde(rename = "ref")]
        #[diff(attr(
            #[derive(Debug, PartialEq)]
        ))]
        git_ref: String,
        #[diff(attr(
            #[derive(Debug, PartialEq)]
        ))]
        from: Option<String>,
    },
    Ref(String),
}

#[derive(clap::Args, Clone, Debug, Diff, Deserialize, PartialEq, Eq, Serialize)]
#[diff(attr(
    #[derive(Debug, PartialEq)]
))]
pub struct TreeSitter {
    /// Tree-sitter version.
    #[arg(short = 'V', long = "tree-sitter-version", default_value = TREE_SITTER_VERSION)]
    pub version: String,

    /// Tree-sitter repo.
    #[arg(short = 'R', long = "tree-sitter-repo", default_value = TREE_SITTER_REPO)]
    pub repo: String,

    /// Tree-sitter platform to build. Change at your own risk.
    #[clap(long = "tree-sitter-platform", default_value = TREE_SITTER_PLATFORM)]
    pub platform: String,
}

impl Default for TreeSitter {
    fn default() -> Self {
        Self {
            version: TREE_SITTER_VERSION.to_string(),
            repo: TREE_SITTER_REPO.to_string(),
            platform: TREE_SITTER_PLATFORM.to_string(),
        }
    }
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
