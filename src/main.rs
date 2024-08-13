use clap::Parser;
use miette::Result;
use tracing::{error, info};

use tsdl::{args, build, config, display, logging};

fn main() -> Result<()> {
    set_panic_hook();
    let args = args::Args::parse();
    let _guard = logging::init(&args)?;
    info!("Starting");
    run(&args)?;
    info!("Done");
    Ok(())
}

fn run(args: &args::Args) -> Result<()> {
    match &args.command {
        args::Command::Build(command) => build::run(
            &config::current(&args.config, Some(command))?,
            display::current(&args.progress),
        ),
        args::Command::Config { command } => config::run(command, &args.config),
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
