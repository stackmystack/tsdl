use std::path::PathBuf;

use clap_verbosity_flag::{InfoLevel, Verbosity};

use crate::{args, config, display, logging, Result};

/// Resolved application state, ready to run.
pub struct App {
    pub subcommand: args::Command,
    pub command: args::BuildCommand,
    pub provenance: config::BuildProvenance,
    pub config_path: PathBuf,
    pub log_path: PathBuf,
    pub progress_mode: display::Mode,
    pub verbose: Verbosity<InfoLevel>,
    pub logging_guard: Option<logging::Guard>,
}

pub fn setup() -> Result<App> {
    let (args, matches) = config::parse_with_matches();
    let build_matches = crate::args::build_matches(&matches);

    let (command, provenance) = config::current_with_provenance(&args.config, build_matches)?;

    let (log_path, logging_guard) = logging::init(
        args.log.as_ref(),
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
        logging_guard: Some(logging_guard),
    })
}
