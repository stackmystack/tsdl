use std::path::PathBuf;

use clap_verbosity_flag::{InfoLevel, Verbosity};

use crate::{args::Args, args::BuildCommand, config, display, TsdlResult};

/// Application containing all resolved configuration and state.
pub struct App {
    pub command: BuildCommand,
    pub config_path: PathBuf,
    pub log_path: PathBuf,
    pub progress_mode: display::Mode,
    pub verbose: Verbosity<InfoLevel>,
}

impl App {
    /// Create application from CLI arguments.
    /// This resolves and merges all configuration sources (CLI, config file, defaults).
    pub fn new(args: &Args, log_path: PathBuf) -> TsdlResult<Self> {
        let command = config::current(&args.config, args.command.as_build())?;
        let progress_mode = display::mode_from_args(&args.progress, &args.verbose);

        Ok(Self {
            command,
            log_path,
            progress_mode,
            config_path: args.config.clone(),
            verbose: args.verbose,
        })
    }
}
