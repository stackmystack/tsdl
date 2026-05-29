//! Application entry point: resolves the build command, acquires locks, and
//! orchestrates the builder.

use std::path::{Path, PathBuf};

use clap::ArgMatches;
use clap_verbosity_flag::{InfoLevel, Verbosity};

use crate::{Result, ResultExt, args, config, display, logging};

// ============================================================
// Enums
// ============================================================

/// The selected command after resolving only the configuration it needs.
pub enum ResolvedCommand {
  Build(ResolvedBuild),
  ConfigCurrent(ResolvedBuild),
  ConfigDefault,
  Selfupdate { force: bool, target: String },
}

// ============================================================
// Structs
// ============================================================

/// Resolved application state, ready to run.
pub struct App {
  pub command: ResolvedCommand,
  pub config_path: PathBuf,
  pub logging: logging::Session,
  pub progress_mode: display::Mode,
  pub verbose: Verbosity<InfoLevel>,
}

/// Resolved build state for commands that actually need build configuration.
pub struct ResolvedBuild {
  pub command: args::BuildCommand,
  pub provenance: config::BuildProvenance,
}

// ============================================================
// Free functions
// ============================================================

/// Resolve the build configuration for a given purpose (e.g. "build", "config
/// current") and return both the resolved command and its provenance.
fn resolve_build(
  config_path: &Path,
  matches: Option<&ArgMatches>,
  purpose: &str,
) -> Result<ResolvedBuild> {
  let (command, provenance) = config::current_with_provenance(config_path, matches)
    .with_context(|| format!("Resolving build configuration for {purpose}"))?;

  Ok(ResolvedBuild {
    command,
    provenance,
  })
}

/// map cli args + matches into a [`resolvedcommand`] (build, config, or self-update).
fn resolve_command(args: &args::Args, matches: &ArgMatches) -> Result<ResolvedCommand> {
  use args::{Command::*, ConfigCommand::*};

  match &args.command {
    | Build => resolve_build(&args.config, args::build_matches(matches), "`build`")
      .map(ResolvedCommand::Build),
    | Config { command: Current } => {
      resolve_build(&args.config, None, "`config current`").map(ResolvedCommand::ConfigCurrent)
    }
    | Config { command: Default } => Ok(ResolvedCommand::ConfigDefault),
    | Selfupdate { force, target } => Ok(ResolvedCommand::Selfupdate {
      force: *force,
      target: target.clone(),
    }),
  }
}

/// Parse CLI args, resolve the command, and initialise logging.
pub fn setup() -> Result<App> {
  let (args, matches) = config::parse_with_matches();
  let command = resolve_command(&args, &matches)?;
  let logging = logging::init(
    command.logging_policy(args.log.as_deref()),
    args.log_color,
    args.verbose,
  )?;

  let progress_mode = display::mode_from_args(&args.progress, &args.verbose);

  Ok(App {
    command,
    config_path: args.config,
    logging,
    progress_mode,
    verbose: args.verbose,
  })
}

// ============================================================
// Impls
// ============================================================

impl ResolvedCommand {
  /// Return a logging policy based on the command type.
  fn logging_policy<'a>(&'a self, explicit: Option<&'a Path>) -> logging::Policy<'a> {
    use logging::{Implicit::*, Policy};

    let implicit = match self {
      | Self::Build(build) => BuildDir {
        dir: &build.command.build_dir,
      },
      | _ => logging::Implicit::None,
    };

    Policy { explicit, implicit }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn parse_with_config(config: &Path, command: &[&str]) -> (args::Args, ArgMatches) {
    let mut argv = vec![
      "tsdl".into(),
      "--config".into(),
      config.as_os_str().to_owned(),
    ];
    argv.extend(command.iter().map(Into::into));
    config::try_parse_from_with_matches(argv).unwrap()
  }

  #[test]
  fn config_default_does_not_resolve_build_config() {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("parsers.toml");
    std::fs::write(&config_path, "not valid toml =").unwrap();

    let (args, matches) = parse_with_config(&config_path, &["config", "default"]);
    let command = resolve_command(&args, &matches).unwrap();

    assert!(matches!(command, ResolvedCommand::ConfigDefault));
  }

  #[test]
  fn selfupdate_does_not_resolve_build_config() {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("parsers.toml");
    std::fs::write(&config_path, "not valid toml =").unwrap();

    let (args, matches) = parse_with_config(&config_path, &["selfupdate"]);
    let command = resolve_command(&args, &matches).unwrap();

    assert!(matches!(command, ResolvedCommand::Selfupdate { .. }));
  }
}
