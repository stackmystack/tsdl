use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    result::Result as StdResult,
};

use clap::{
    parser::ValueSource, value_parser, Arg, ArgAction, ArgMatches, CommandFactory, FromArgMatches,
};
use serde::Serialize;
use tracing::debug;

use crate::{args, columns, Result, ResultExt};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    #[default]
    BuiltInDefault,
    ConfigFile,
    Environment,
    CommandLine,
}

impl Source {
    fn from_value_source(source: ValueSource) -> Option<Self> {
        match source {
            ValueSource::CommandLine => Some(Self::CommandLine),
            ValueSource::EnvVariable => Some(Self::Environment),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TreeSitterProvenance {
    pub version: Source,
    pub platform: Source,
    pub repo: Source,
}

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

pub fn current(config: &Path, matches: Option<&ArgMatches>) -> Result<args::BuildCommand> {
    let (cmd, _provenance) = current_with_provenance(config, matches)?;
    Ok(cmd)
}

pub fn current_with_provenance(
    config: &Path,
    matches: Option<&ArgMatches>,
) -> Result<(args::BuildCommand, BuildProvenance)> {
    let defaults = args::BuildCommand::default();
    let file_overrides = read_file_overrides(config)?;
    let file_provenance = file_provenance_from(&file_overrides);

    let (cli_overrides, cli_provenance) = if let Some(matches) = matches {
        extract_overrides(matches, &defaults)
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

fn apply_opt<T>(field: &mut T, value: Option<T>) {
    if let Some(v) = value {
        *field = v;
    }
}

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
            version: merge_source(file.tree_sitter.version, cli.tree_sitter.version),
            platform: merge_source(file.tree_sitter.platform, cli.tree_sitter.platform),
            repo: merge_source(file.tree_sitter.repo, cli.tree_sitter.repo),
        },
        unlock_timeout: merge_source(file.unlock_timeout, cli.unlock_timeout),
    }
}

fn merge_source(file: Source, cli: Source) -> Source {
    if cli == Source::default() {
        file
    } else {
        cli
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn build_cli(defaults: &args::BuildCommand) -> Vec<Arg> {
    let jobs_default = defaults.jobs.to_string();
    let ut_default = defaults.unlock_timeout.to_string();

    vec![
        Arg::new("build-dir")
            .long("build-dir")
            .short('b')
            .env("BUILD_DIR")
            .value_parser(value_parser!(PathBuf))
            .help(format!(
                "Build Directory [default: {}]",
                defaults.build_dir.display()
            )),
        Arg::new("force")
            .long("force")
            .env("FORCE")
            .num_args(0..=1)
            .require_equals(true)
            .default_missing_value("true")
            .value_parser(value_parser!(bool))
            .help("Force clone the repository and rebuild, bypassing cache checks"),
        Arg::new("no-force")
            .long("no-force")
            .action(ArgAction::SetTrue)
            .help("Disable --force, overriding config files or environment variables"),
        Arg::new("fresh")
            .long("fresh")
            .short('f')
            .env("FRESH")
            .num_args(0..=1)
            .require_equals(true)
            .default_missing_value("true")
            .value_parser(value_parser!(bool))
            .help("Clears the build-dir and starts a fresh build"),
        Arg::new("no-fresh")
            .long("no-fresh")
            .action(ArgAction::SetTrue)
            .help("Disable --fresh, overriding config files or environment variables"),
        Arg::new("languages")
            .num_args(0..)
            .value_parser(value_parser!(String))
            .help("Parsers to compile"),
        Arg::new("jobs")
            .long("jobs")
            .short('j')
            .env("TSDL_NCPUS")
            .value_parser(value_parser!(usize))
            .help(format!("Number of threads [default: {jobs_default}]")),
        Arg::new("out-dir")
            .long("out-dir")
            .short('o')
            .env("PARSER_OUT_DIR")
            .value_parser(value_parser!(PathBuf))
            .help(format!(
                "Output Directory [default: {}]",
                defaults.out_dir.display()
            )),
        Arg::new("prefix")
            .long("prefix")
            .short('p')
            .env("PREFIX")
            .value_parser(value_parser!(String))
            .help(format!(
                "Prefix parser names [default: {}]",
                defaults.prefix
            )),
        Arg::new("show-config")
            .long("show-config")
            .env("SHOW_CONFIG")
            .num_args(0..=1)
            .require_equals(true)
            .default_missing_value("true")
            .value_parser(value_parser!(bool))
            .help("Show Config"),
        Arg::new("no-show-config")
            .long("no-show-config")
            .action(ArgAction::SetTrue)
            .help("Disable --show-config, overriding config files or environment variables"),
        Arg::new("target")
            .long("target")
            .short('t')
            .env("TSDL_TARGET")
            .value_parser([
                clap::builder::PossibleValue::new("native"),
                clap::builder::PossibleValue::new("wasm"),
                clap::builder::PossibleValue::new("all"),
            ])
            .help(format!(
                "Build target [default: {}]",
                defaults.target.to_lowercase()
            )),
        Arg::new("tree-sitter-version")
            .long("tree-sitter-version")
            .short('V')
            .env("TSDL_VERSION")
            .value_parser(value_parser!(String))
            .help(format!(
                "Tree-sitter version [default: {}]",
                defaults.tree_sitter.version
            )),
        Arg::new("tree-sitter-platform")
            .long("tree-sitter-platform")
            .env("TSDL_PLATFORM")
            .value_parser(value_parser!(String))
            .help(format!(
                "Tree-sitter platform to build [default: {}]",
                defaults.tree_sitter.platform
            )),
        Arg::new("tree-sitter-repo")
            .long("tree-sitter-repo")
            .short('R')
            .env("TSDL_REPO")
            .value_parser(value_parser!(String))
            .help(format!(
                "Tree-sitter repo [default: {}]",
                defaults.tree_sitter.repo
            )),
        Arg::new("unlock-timeout")
            .long("unlock-timeout")
            .env("UNLOCK_TIMEOUT")
            .value_parser(value_parser!(u64).range(1..))
            .help(format!(
                "Seconds to wait after terminating a lock owner [default: {ut_default}]"
            )),
    ]
}

#[must_use]
pub fn extract_overrides(
    matches: &ArgMatches,
    _defaults: &args::BuildCommand,
) -> (args::OptionalBuildCommand, BuildProvenance) {
    let mut o = args::OptionalBuildCommand::default();
    let mut p = BuildProvenance::default();

    extract_simple(matches, "build-dir", &mut o.build_dir, &mut p.build_dir);
    extract_bool(matches, "force", "no-force", &mut o.force, &mut p.force);
    extract_bool(matches, "fresh", "no-fresh", &mut o.fresh, &mut p.fresh);

    if let Some(source) = source_for(matches, "languages") {
        let vals: Vec<String> = matches
            .get_many::<String>("languages")
            .map(|v| v.cloned().collect())
            .unwrap_or_default();
        if !vals.is_empty() {
            o.languages = Some(vals);
            p.languages = source;
        }
    }

    extract_simple_usize(matches, "jobs", &mut o.jobs, &mut p.jobs);
    extract_simple(matches, "out-dir", &mut o.out_dir, &mut p.out_dir);
    extract_simple(matches, "prefix", &mut o.prefix, &mut p.prefix);

    extract_bool(
        matches,
        "show-config",
        "no-show-config",
        &mut o.show_config,
        &mut p.show_config,
    );

    extract_target(matches, "target", &mut o.target, &mut p.target);

    extract_simple(
        matches,
        "tree-sitter-version",
        &mut o.tree_sitter.version,
        &mut p.tree_sitter.version,
    );
    extract_simple(
        matches,
        "tree-sitter-platform",
        &mut o.tree_sitter.platform,
        &mut p.tree_sitter.platform,
    );
    extract_simple(
        matches,
        "tree-sitter-repo",
        &mut o.tree_sitter.repo,
        &mut p.tree_sitter.repo,
    );

    extract_simple_u64(
        matches,
        "unlock-timeout",
        &mut o.unlock_timeout,
        &mut p.unlock_timeout,
    );

    (o, p)
}

fn extract_simple<T: Clone + Send + Sync + 'static>(
    matches: &ArgMatches,
    id: &str,
    field: &mut Option<T>,
    provenance: &mut Source,
) {
    if let Some(source) = source_for(matches, id) {
        if let Some(val) = matches.get_one::<T>(id) {
            *field = Some(val.clone());
            *provenance = source;
        }
    }
}

fn extract_simple_usize(
    matches: &ArgMatches,
    id: &str,
    field: &mut Option<std::num::NonZeroUsize>,
    provenance: &mut Source,
) {
    if let Some(source) = source_for(matches, id) {
        if let Some(&n) = matches.get_one::<usize>(id) {
            if let Some(nz) = std::num::NonZeroUsize::new(n) {
                *field = Some(nz);
                *provenance = source;
            }
        }
    }
}

fn extract_simple_u64(
    matches: &ArgMatches,
    id: &str,
    field: &mut Option<u64>,
    provenance: &mut Source,
) {
    if let Some(source) = source_for(matches, id) {
        if let Some(&val) = matches.get_one::<u64>(id) {
            *field = Some(val);
            *provenance = source;
        }
    }
}

fn extract_target(
    matches: &ArgMatches,
    id: &str,
    field: &mut Option<args::Target>,
    provenance: &mut Source,
) {
    if let Some(source) = source_for(matches, id) {
        if let Some(raw) = matches.get_one::<String>(id) {
            match raw.to_lowercase().as_str() {
                "native" => {
                    *field = Some(args::Target::Native);
                    *provenance = source;
                }
                "wasm" => {
                    *field = Some(args::Target::Wasm);
                    *provenance = source;
                }
                "all" => {
                    *field = Some(args::Target::All);
                    *provenance = source;
                }
                _ => {}
            }
        }
    }
}

fn extract_bool(
    matches: &ArgMatches,
    positive_id: &str,
    negative_id: &str,
    field: &mut Option<bool>,
    provenance: &mut Source,
) {
    let pos_source = source_for(matches, positive_id);
    let neg_source = source_for(matches, negative_id);

    if matches!(neg_source, Some(Source::CommandLine)) {
        *field = Some(false);
        *provenance = Source::CommandLine;
        return;
    }

    if let Some(source) = pos_source {
        if let Some(&val) = matches.get_one::<bool>(positive_id) {
            *field = Some(val);
            *provenance = source;
        }
    }
}

fn source_for(matches: &ArgMatches, id: &str) -> Option<Source> {
    matches.value_source(id).and_then(Source::from_value_source)
}

pub fn print_indent(s: &str, indent: &str) {
    s.lines().for_each(|line| println!("{indent}{line}"));
}

pub fn run(config_path: &Path, command: &args::ConfigCommand) -> Result<()> {
    match command {
        args::ConfigCommand::Current => {
            let cmd: args::BuildCommand = current(config_path, None)?;
            println!(
                "{}",
                toml::to_string(&cmd).context("Generating default TOML config")?
            );
        }
        args::ConfigCommand::Default => println!(
            "{}",
            toml::to_string(&args::BuildCommand::default())
                .context("Generating default TOML config")?
        ),
    }
    Ok(())
}

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

#[must_use]
pub fn parse_with_matches() -> (args::Args, ArgMatches) {
    let defaults = args::BuildCommand::default();
    let build_args = build_cli(&defaults);
    let mut cmd = args::Args::command();
    if let Some(build_sub) = cmd.find_subcommand_mut("build") {
        let mut new_sub = build_sub.clone();
        for arg in build_args {
            new_sub = new_sub.arg(arg);
        }
        *build_sub = new_sub;
    }
    let matches = cmd.get_matches();
    let args = args::Args::from_arg_matches(&matches).unwrap_or_else(|err| err.exit());
    (args, matches)
}

pub fn try_parse_from_with_matches<I, T>(itr: I) -> StdResult<(args::Args, ArgMatches), clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let defaults = args::BuildCommand::default();
    let build_args = build_cli(&defaults);
    let mut cmd = args::Args::command();
    if let Some(build_sub) = cmd.find_subcommand_mut("build") {
        let mut new_sub = build_sub.clone();
        for arg in build_args {
            new_sub = new_sub.arg(arg);
        }
        *build_sub = new_sub;
    }
    let matches = cmd.try_get_matches_from(itr)?;
    let args = args::Args::from_arg_matches(&matches)?;
    Ok((args, matches))
}
