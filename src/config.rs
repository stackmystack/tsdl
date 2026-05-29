//! Configuration merging: CLI flags ⊕ `parsers.toml` ⊕ defaults, resolved via
//! figment and diff-struct.

use std::{
    ffi::OsString,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    result::Result as StdResult,
};

use clap::{ArgMatches, Args, CommandFactory, FromArgMatches, parser::ValueSource};
use serde::Serialize;
use tracing::debug;

use crate::{Result, ResultExt, args, columns};

// ============================================================
// Constants
// ============================================================

// clap derive uses the Rust field name as the arg ID, not the long flag
// name.  These constants match the field names so `ArgMatches::value_source`
// can look them up.

const ARG_BUILD_DIR: &str = "build_dir";
const ARG_FORCE: &str = "force";
const ARG_NO_FORCE: &str = "no_force";
const ARG_FRESH: &str = "fresh";
const ARG_NO_FRESH: &str = "no_fresh";
const ARG_LANGUAGES: &str = "languages";
const ARG_JOBS: &str = "jobs";
const ARG_OUT_DIR: &str = "out_dir";
const ARG_PREFIX: &str = "prefix";
const ARG_SHOW_CONFIG: &str = "show_config";
const ARG_NO_SHOW_CONFIG: &str = "no_show_config";
const ARG_TARGET: &str = "target";
const ARG_TS_VERSION: &str = "version";
const ARG_TS_PLATFORM: &str = "platform";
const ARG_TS_REPO: &str = "repo";
const ARG_UNLOCK_TIMEOUT: &str = "unlock_timeout";

// ============================================================
// Enums
// ============================================================

/// Provenance source for a single configuration value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// Compiled-in default.
    #[default]
    BuiltInDefault,
    /// From the parser TOML config file.
    ConfigFile,
    /// From an environment variable.
    Environment,
    /// From a CLI flag.
    CommandLine,
}

// ============================================================
// Structs
// ============================================================

/// CLI arguments for the `build` subcommand, defined via derive so the arg
/// definitions and value extraction stay in sync.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct BuildArgs {
    /// Override build directory (`--build-dir`, `-b`).
    #[arg(long = "build-dir", short = 'b', env = "BUILD_DIR")]
    pub build_dir: Option<PathBuf>,

    /// Force rebuild (`--force`).
    #[arg(
        long = "force",
        env = "FORCE",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true"
    )]
    pub force: Option<bool>,
    /// Negate --force (`--no-force`).
    #[arg(long = "no-force")]
    pub no_force: bool,

    /// Fresh build, clear build directory (`--fresh`, `-f`).
    #[arg(
        long = "fresh",
        short = 'f',
        env = "FRESH",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true"
    )]
    pub fresh: Option<bool>,
    /// Negate --fresh (`--no-fresh`).
    #[arg(long = "no-fresh")]
    pub no_fresh: bool,

    /// Languages to build (positional args, zero or more).
    #[arg(num_args = 0..)]
    pub languages: Vec<String>,

    /// Max concurrent jobs (`--jobs`, `-j`).
    #[arg(long = "jobs", short = 'j', env = "TSDL_NCPUS")]
    pub jobs: Option<usize>,

    /// Output directory for installed binaries (`--out-dir`, `-o`).
    #[arg(long = "out-dir", short = 'o', env = "PARSER_OUT_DIR")]
    pub out_dir: Option<PathBuf>,

    /// Output filename prefix (`--prefix`, `-p`).
    #[arg(long = "prefix", short = 'p', env = "PREFIX")]
    pub prefix: Option<String>,

    /// Print resolved config and exit (`--show-config`).
    #[arg(
        long = "show-config",
        env = "SHOW_CONFIG",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true"
    )]
    pub show_config: Option<bool>,
    /// Negate --show-config (`--no-show-config`).
    #[arg(long = "no-show-config")]
    pub no_show_config: bool,

    /// Build target (`--target`, `-t`).
    #[arg(long = "target", short = 't', env = "TSDL_TARGET", value_enum)]
    pub target: Option<args::Target>,

    #[command(flatten)]
    pub tree_sitter: TreeSitterArgs,

    /// Lock wait timeout in seconds (`--unlock-timeout`).
    #[arg(
        long = "unlock-timeout",
        env = "UNLOCK_TIMEOUT",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub unlock_timeout: Option<u64>,
}

/// Tracks the source provenance for every field in [`args::BuildCommand`].
///
/// Each field records whether the corresponding [`args::BuildCommand`]
/// value came from the built-in default, the config file, an env var, or
/// the CLI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct BuildProvenance {
    pub build_dir: Source,
    pub force: Source,
    pub fresh: Source,
    pub languages: Source,
    pub jobs: Source,
    pub out_dir: Source,
    pub parsers: Source,
    pub prefix: Source,
    pub show_config: Source,
    pub target: Source,
    pub tree_sitter: TreeSitterProvenance,
    pub unlock_timeout: Source,
}

/// Nested CLI arguments for tree-sitter configuration.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct TreeSitterArgs {
    /// CLI flag: `--tree-sitter-version` / `-V`.
    #[arg(long = "tree-sitter-version", short = 'V', env = "TSDL_VERSION")]
    pub version: Option<String>,
    /// CLI flag: `--tree-sitter-platform`.
    #[arg(long = "tree-sitter-platform", env = "TSDL_PLATFORM")]
    pub platform: Option<String>,
    /// CLI flag: `--tree-sitter-repo` / `-R`.
    #[arg(long = "tree-sitter-repo", short = 'R', env = "TSDL_REPO")]
    pub repo: Option<String>,
}

/// Tracks the source provenance for every field in [`args::TreeSitter`].
///
/// Each field records whether the corresponding [`args::TreeSitter`]
/// value came from the built-in default, the config file, an env var, or
/// the CLI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TreeSitterProvenance {
    pub version: Source,
    pub platform: Source,
    pub repo: Source,
}

// ============================================================
// Free functions
// ============================================================

/// Apply an Option value to a mutable field if present.
fn apply_opt<T>(field: &mut T, value: Option<T>) {
    if let Some(v) = value {
        *field = v;
    }
}

/// Resolve the current merged configuration from defaults, config file, and CLI.
pub fn current(config: &Path, matches: Option<&ArgMatches>) -> Result<args::BuildCommand> {
    let (cmd, _provenance) = current_with_provenance(config, matches)?;
    Ok(cmd)
}

/// Resolve the current merged configuration with full provenance tracking.
pub fn current_with_provenance(
    config: &Path,
    matches: Option<&ArgMatches>,
) -> Result<(args::BuildCommand, BuildProvenance)> {
    let defaults = args::BuildCommand::default();
    let file_overrides = read_file_overrides(config)?;
    let file_provenance = file_provenance_from(&file_overrides);

    let (cli_overrides, cli_provenance) = if let Some(matches) = matches {
        extract_overrides(matches)
    } else {
        (
            args::OptionalBuildCommand::default(),
            BuildProvenance::default(),
        )
    };

    let command = merge(defaults, file_overrides, cli_overrides);
    let provenance = merge_provenance(&file_provenance, &cli_provenance);

    debug!(?provenance, ?command, "Resolved build configuration");

    Ok((command, provenance))
}

/// Extract CLI overrides from parsed `ArgMatches` into `OptionalBuildCommand`
/// with provenance tracking.
#[must_use]
pub fn extract_overrides(matches: &ArgMatches) -> (args::OptionalBuildCommand, BuildProvenance) {
    let cli = BuildArgs::from_arg_matches(matches).unwrap_or_else(|err| err.exit());
    overrides_from_build_args(cli, matches)
}

/// Build provenance for each config file override.
fn file_provenance_from(overrides: &args::OptionalBuildCommand) -> BuildProvenance {
    let mut p = BuildProvenance::default();
    if overrides.build_dir.is_some() {
        p.build_dir = Source::ConfigFile;
    }
    if overrides.force.is_some() {
        p.force = Source::ConfigFile;
    }
    if overrides.fresh.is_some() {
        p.fresh = Source::ConfigFile;
    }
    if overrides.jobs.is_some() {
        p.jobs = Source::ConfigFile;
    }
    if overrides.out_dir.is_some() {
        p.out_dir = Source::ConfigFile;
    }
    if overrides.parsers.is_some() {
        p.parsers = Source::ConfigFile;
    }
    if overrides.prefix.is_some() {
        p.prefix = Source::ConfigFile;
    }
    if overrides.show_config.is_some() {
        p.show_config = Source::ConfigFile;
    }
    if overrides.target.is_some() {
        p.target = Source::ConfigFile;
    }
    if overrides.unlock_timeout.is_some() {
        p.unlock_timeout = Source::ConfigFile;
    }
    let ts = &overrides.tree_sitter;
    if ts.version.is_some() {
        p.tree_sitter.version = Source::ConfigFile;
    }
    if ts.platform.is_some() {
        p.tree_sitter.platform = Source::ConfigFile;
    }
    if ts.repo.is_some() {
        p.tree_sitter.repo = Source::ConfigFile;
    }
    p
}

/// Merge defaults + file overrides + CLI overrides into a final `BuildCommand`.
fn merge(
    defaults: args::BuildCommand,
    file: args::OptionalBuildCommand,
    cli: args::OptionalBuildCommand,
) -> args::BuildCommand {
    let mut cmd = defaults;
    apply_opt(&mut cmd.build_dir, file.build_dir);
    apply_opt(&mut cmd.build_dir, cli.build_dir);
    apply_opt(&mut cmd.force, file.force);
    apply_opt(&mut cmd.force, cli.force);
    apply_opt(&mut cmd.fresh, file.fresh);
    apply_opt(&mut cmd.fresh, cli.fresh);
    apply_opt(&mut cmd.jobs, file.jobs);
    apply_opt(&mut cmd.jobs, cli.jobs);
    apply_opt(&mut cmd.out_dir, file.out_dir);
    apply_opt(&mut cmd.out_dir, cli.out_dir);
    if let Some(parsers) = file.parsers {
        cmd.parsers = Some(parsers);
    }
    apply_opt(&mut cmd.prefix, file.prefix);
    apply_opt(&mut cmd.prefix, cli.prefix);
    apply_opt(&mut cmd.show_config, file.show_config);
    apply_opt(&mut cmd.show_config, cli.show_config);
    apply_opt(&mut cmd.target, file.target);
    apply_opt(&mut cmd.target, cli.target);
    apply_opt(&mut cmd.unlock_timeout, file.unlock_timeout);
    apply_opt(&mut cmd.unlock_timeout, cli.unlock_timeout);

    apply_opt(&mut cmd.tree_sitter.version, file.tree_sitter.version);
    apply_opt(&mut cmd.tree_sitter.version, cli.tree_sitter.version);
    apply_opt(&mut cmd.tree_sitter.platform, file.tree_sitter.platform);
    apply_opt(&mut cmd.tree_sitter.platform, cli.tree_sitter.platform);
    apply_opt(&mut cmd.tree_sitter.repo, file.tree_sitter.repo);
    apply_opt(&mut cmd.tree_sitter.repo, cli.tree_sitter.repo);

    if let Some(langs) = cli.languages {
        cmd.languages = Some(langs);
    }

    cmd
}

/// Merge file and CLI provenance into a single resolved provenance.
fn merge_provenance(file: &BuildProvenance, cli: &BuildProvenance) -> BuildProvenance {
    BuildProvenance {
        build_dir: merge_source(file.build_dir, cli.build_dir),
        force: merge_source(file.force, cli.force),
        fresh: merge_source(file.fresh, cli.fresh),
        languages: merge_source(file.languages, cli.languages),
        jobs: merge_source(file.jobs, cli.jobs),
        out_dir: merge_source(file.out_dir, cli.out_dir),
        parsers: merge_source(file.parsers, cli.parsers),
        prefix: merge_source(file.prefix, cli.prefix),
        show_config: merge_source(file.show_config, cli.show_config),
        target: merge_source(file.target, cli.target),
        tree_sitter: TreeSitterProvenance {
            platform: merge_source(file.tree_sitter.platform, cli.tree_sitter.platform),
            repo: merge_source(file.tree_sitter.repo, cli.tree_sitter.repo),
            version: merge_source(file.tree_sitter.version, cli.tree_sitter.version),
        },
        unlock_timeout: merge_source(file.unlock_timeout, cli.unlock_timeout),
    }
}

/// Pick the CLI source if set, otherwise fall back to file source.
fn merge_source(file: Source, cli: Source) -> Source {
    if cli == Source::default() { file } else { cli }
}

/// Convert CLI `BuildArgs` into `OptionalBuildCommand` + `BuildProvenance`.
fn overrides_from_build_args(
    cli: BuildArgs,
    matches: &ArgMatches,
) -> (args::OptionalBuildCommand, BuildProvenance) {
    let mut o = args::OptionalBuildCommand::default();
    let mut p = BuildProvenance::default();

    set_simple(
        matches,
        ARG_BUILD_DIR,
        cli.build_dir,
        &mut o.build_dir,
        &mut p.build_dir,
    );
    resolve_bool(
        matches,
        cli.force,
        ARG_FORCE,
        ARG_NO_FORCE,
        &mut o.force,
        &mut p.force,
    );
    resolve_bool(
        matches,
        cli.fresh,
        ARG_FRESH,
        ARG_NO_FRESH,
        &mut o.fresh,
        &mut p.fresh,
    );

    if !cli.languages.is_empty()
        && let Some(source) = source_for(matches, ARG_LANGUAGES)
    {
        o.languages = Some(cli.languages);
        p.languages = source;
    }

    if let Some(n) = cli.jobs.and_then(NonZeroUsize::new)
        && let Some(source) = source_for(matches, ARG_JOBS)
    {
        o.jobs = Some(n);
        p.jobs = source;
    }

    set_simple(
        matches,
        ARG_OUT_DIR,
        cli.out_dir,
        &mut o.out_dir,
        &mut p.out_dir,
    );
    set_simple(
        matches,
        ARG_PREFIX,
        cli.prefix,
        &mut o.prefix,
        &mut p.prefix,
    );

    resolve_bool(
        matches,
        cli.show_config,
        ARG_SHOW_CONFIG,
        ARG_NO_SHOW_CONFIG,
        &mut o.show_config,
        &mut p.show_config,
    );

    set_simple(
        matches,
        ARG_TARGET,
        cli.target,
        &mut o.target,
        &mut p.target,
    );

    set_simple(
        matches,
        ARG_TS_VERSION,
        cli.tree_sitter.version,
        &mut o.tree_sitter.version,
        &mut p.tree_sitter.version,
    );
    set_simple(
        matches,
        ARG_TS_PLATFORM,
        cli.tree_sitter.platform,
        &mut o.tree_sitter.platform,
        &mut p.tree_sitter.platform,
    );
    set_simple(
        matches,
        ARG_TS_REPO,
        cli.tree_sitter.repo,
        &mut o.tree_sitter.repo,
        &mut p.tree_sitter.repo,
    );

    set_simple(
        matches,
        ARG_UNLOCK_TIMEOUT,
        cli.unlock_timeout,
        &mut o.unlock_timeout,
        &mut p.unlock_timeout,
    );

    (o, p)
}

/// Parse CLI args using the augmented command definitions.
#[must_use]
pub fn parse_with_matches() -> (args::Args, ArgMatches) {
    let mut cmd = args::Args::command();
    if let Some(build_sub) = cmd.find_subcommand_mut("build") {
        *build_sub = BuildArgs::augment_args(build_sub.clone());
    }
    let matches = cmd.get_matches();
    let args = args::Args::from_arg_matches(&matches).unwrap_or_else(|err| err.exit());
    (args, matches)
}

/// Print the current merged configuration as TOML.
pub fn print_current(command: &args::BuildCommand) -> Result<()> {
    println!(
        "{}",
        toml::to_string(command).context("Generating current TOML config")?
    );
    Ok(())
}

/// Print the built-in default configuration as TOML.
pub fn print_default() -> Result<()> {
    println!(
        "{}",
        toml::to_string(&args::BuildCommand::default())
            .context("Generating default TOML config")?
    );
    Ok(())
}

/// Print a string with a per-line indent prefix.
pub fn print_indent(s: &str, indent: &str) {
    s.lines().for_each(|line| println!("{indent}{line}"));
}

/// Read parser config file overrides from the TOML file at `config`.
fn read_file_overrides(config: &Path) -> Result<args::OptionalBuildCommand> {
    if !config.exists() {
        return Ok(args::OptionalBuildCommand::default());
    }

    let contents = fs::read_to_string(config)
        .with_context(|| format!("Reading config file {}", config.display()))?;

    if contents.trim().is_empty() {
        return Ok(args::OptionalBuildCommand::default());
    }

    toml::from_str(&contents).with_context(|| format!("Parsing config file {}", config.display()))
}

/// Resolve a boolean field that has both a positive (`--force`) and negative
/// (`--no-force`) flag.  The negative form only takes effect when passed
/// explicitly on the command line; env-var / config values flow through the
/// positive arg.
fn resolve_bool(
    matches: &ArgMatches,
    positive: Option<bool>,
    pos_id: &str,
    neg_id: &str,
    field: &mut Option<bool>,
    provenance: &mut Source,
) {
    // --no-* on the command line always wins.
    if matches!(source_for(matches, neg_id), Some(Source::CommandLine)) {
        *field = Some(false);
        *provenance = Source::CommandLine;
        return;
    }

    if let Some(source) = source_for(matches, pos_id)
        && let Some(val) = positive
    {
        *field = Some(val);
        *provenance = source;
    }
}

/// Run a config command (current or default).
pub fn run(config_path: &Path, command: &args::ConfigCommand) -> Result<()> {
    match command {
        args::ConfigCommand::Current => print_current(&current(config_path, None)?),
        args::ConfigCommand::Default => print_default(),
    }
}

/// Set a simple `Option<T>` override when the arg's `ValueSource` indicates
/// the user supplied it (CLI or env).
fn set_simple<T>(
    matches: &ArgMatches,
    id: &str,
    value: Option<T>,
    field: &mut Option<T>,
    provenance: &mut Source,
) {
    if let Some(source) = source_for(matches, id)
        && let Some(val) = value
    {
        *field = Some(val);
        *provenance = source;
    }
}

/// Print a human-readable summary of the resolved configuration.
pub fn show(command: &args::BuildCommand) -> Result<()> {
    if let Some(langs) = &command.languages {
        println!("Building the following languages:");
        println!();
        print!("{}", columns::format_git(langs, "  ", 80));
        println!();
    } else {
        println!("Building all languages.");
        println!();
    }
    println!("Running with the following configuration:");
    println!();
    print_indent(&toml::to_string(&command).context("Showing config")?, "  ");
    println!();
    Ok(())
}

/// Look up the `ValueSource` for a given arg ID and convert it to a Source.
fn source_for(matches: &ArgMatches, id: &str) -> Option<Source> {
    matches.value_source(id).and_then(Source::from_value_source)
}

/// Parse CLI args from an iterator (used in tests).
pub fn try_parse_from_with_matches<I, T>(itr: I) -> StdResult<(args::Args, ArgMatches), clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let mut cmd = args::Args::command();
    if let Some(build_sub) = cmd.find_subcommand_mut("build") {
        *build_sub = BuildArgs::augment_args(build_sub.clone());
    }
    let matches = cmd.try_get_matches_from(itr)?;
    let args = args::Args::from_arg_matches(&matches)?;
    Ok((args, matches))
}

// ============================================================
// Impls
// ============================================================

impl Source {
    /// Convert a clap `ValueSource` to a config Source.
    fn from_value_source(source: ValueSource) -> Option<Self> {
        match source {
            ValueSource::CommandLine => Some(Self::CommandLine),
            ValueSource::EnvVariable => Some(Self::Environment),
            _ => None,
        }
    }
}
