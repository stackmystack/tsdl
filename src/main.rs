use std::{process::ExitCode, time::Instant};

use console::style;
use tracing::{error, info};

use tsdl::{TsdlResult, app, args, error::TsdlError};

fn main() -> ExitCode {
    set_panic_hook();
    let app = match app::setup() {
        Ok(app) => app,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    info!("Starting");
    match run(app) {
        Err(TsdlError::Interrupted(signal)) => ExitCode::from(signal.shell_exit_code()),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
        Ok(()) => ExitCode::SUCCESS,
    }
}

fn run(app: app::App) -> TsdlResult<()> {
    match app.subcommand {
        tsdl::args::Command::Build => {
            let (result, duration) = time(|| tsdl::build::run(&app));
            match &result {
                Ok(()) => println!("{}", style(format!("Done in {duration}")).green()),
                Err(TsdlError::Interrupted(signal)) => println!(
                    "{}",
                    style(format!("Interrupted by {signal} after {duration}")).yellow()
                ),
                Err(_) => println!("{}", style(format!("Done in {duration}")).red()),
            }
            result
        }
        args::Command::Config { command } => tsdl::config::run(&app.config_path, &command),
        args::Command::Selfupdate{ force, target} => tsdl::selfupdate::run(force, target.as_str()),
    }
}

pub fn set_panic_hook() {
    std::panic::set_hook(Box::new(move |info| {
        #[cfg(not(debug_assertions))]
        {
            use human_panic::{handle_dump, print_msg, Metadata};
            let meta = Metadata::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
                .authors(env!("CARGO_PKG_AUTHORS").replace(':', ", "))
                .homepage(env!("CARGO_PKG_HOMEPAGE"));
            let file_path = handle_dump(&meta, info);
            print_msg(file_path, &meta)
                .expect("human-panic: printing error message to console failed");
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
