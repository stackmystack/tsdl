use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use clap_verbosity_flag::{InfoLevel, Verbosity};

use crate::{args::Args, args::BuildCommand, config, display, TsdlResult};

/// Application containing all resolved configuration and state.
pub struct App {
    pub config: BuildCommand,
    pub progress: Arc<Mutex<display::Progress>>,
    pub config_path: PathBuf,
    pub verbose: Verbosity<InfoLevel>,
}

impl App {
    /// Create application from CLI arguments.
    /// This resolves and merges all configuration sources (CLI, config file, defaults).
    pub fn new(args: &Args) -> TsdlResult<Self> {
        let config = config::current(&args.config, args.command.as_build())?;
        let progress = Arc::new(Mutex::new(display::current(&args.progress, &args.verbose)));

        Ok(Self {
            config,
            progress,
            config_path: args.config.clone(),
            verbose: args.verbose,
        })
    }
}
