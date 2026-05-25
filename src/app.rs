use std::path::PathBuf;

use clap_verbosity_flag::{InfoLevel, Verbosity};

use crate::{
    args::{BuildCommand, Command},
    config, display, logging,
    TsdlResult,
};

/// Resolved application state, ready to run.
pub struct App {
    pub subcommand: Command,
    pub command: BuildCommand,
    pub provenance: config::BuildProvenance,
    pub config_path: PathBuf,
    pub log_path: PathBuf,
    pub progress_mode: display::Mode,
    pub verbose: Verbosity<InfoLevel>,
    pub _logging: Option<logging::Guard>,
}

pub fn setup() -> TsdlResult<App> {
    let (args, matches) = config::parse_with_matches();
    let build_matches = crate::args::build_matches(&matches);

    let (command, provenance) =
        config::current_with_provenance(&args.config, build_matches)?;

    let (log_path, _logging) = logging::init(
        args.log.clone(),
        args.log_color,
        args.verbose,
        &command.build_dir,
    )?;

    let progress_mode = display::mode_from_args(&args.progress, &args.verbose);

    Ok(App {
        subcommand: args.command,
        command,
        provenance,
        config_path: args.config,
        log_path,
        progress_mode,
        verbose: args.verbose,
        _logging: Some(_logging),
    })
}
