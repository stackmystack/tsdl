use std::{process::ExitCode, time::Instant};

use console::style;
use tracing::{error, info};
use tsdl::{Error, Result, app};

fn main() -> ExitCode {
  set_panic_hook();
  let app = match app::setup() {
    | Err(e) => {
      eprintln!("{e}");
      return ExitCode::FAILURE;
    }
    | Ok(app) => app,
  };

  info!("Starting");
  match run(&app) {
    | Err(Error::Interrupted { signal }) => ExitCode::from(signal.shell_exit_code()),
    | Err(e) => {
      eprintln!("{e}");
      ExitCode::FAILURE
    }
    | Ok(()) => ExitCode::SUCCESS,
  }
}

fn run(app: &app::App) -> Result<()> {
  use app::ResolvedCommand::*;

  match &app.command {
    | Build(build) => {
      let (result, duration) = time(|| tsdl::build::run(&build.command, app));
      match &result {
        | Ok(()) => println!("{}", style(format!("Done in {duration}")).green()),
        | Err(Error::Interrupted { signal }) => println!(
          "{}",
          style(format!("Interrupted by {signal} after {duration}")).yellow()
        ),
        | Err(_) => println!("{}", style(format!("Done in {duration}")).red()),
      }
      result
    }
    | ConfigCurrent(build) => tsdl::config::print_current(&build.command),
    | ConfigDefault => tsdl::config::print_default(),
    | Selfupdate { force, target } => tsdl::selfupdate::run(*force, target.as_str()),
  }
}

pub fn set_panic_hook() {
  std::panic::set_hook(Box::new(move |info| {
    #[cfg(not(debug_assertions))]
    {
      use human_panic::{Metadata, handle_dump, print_msg};
      let meta = Metadata::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
        .authors(env!("CARGO_PKG_AUTHORS").replace(':', ", "))
        .homepage(env!("CARGO_PKG_HOMEPAGE"));
      let file_path = handle_dump(&meta, info);
      print_msg(file_path, &meta).expect("human-panic: printing error message to console failed");
    }
    #[cfg(debug_assertions)]
    {
      better_panic::Settings::auto()
        .most_recent_first(false)
        .lineno_suffix(true)
        .verbosity(better_panic::Verbosity::Full)
        .create_panic_handler()(info);
    }
    error!("{}", info);
    std::process::exit(1);
  }));
}

fn time<F, T>(f: F) -> (T, String)
where
  F: FnOnce() -> T,
{
  let start = Instant::now();
  let result = f();
  (result, tsdl::format_duration(start.elapsed()))
}
