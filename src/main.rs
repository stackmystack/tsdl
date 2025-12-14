use std::{fs, path::PathBuf, process::ExitCode};

use clap::Parser;
use self_update::self_replace;
use semver::Version;
use tracing::{error, info};

use tsdl::{
    app::App,
    args,
    consts::TREE_SITTER_PLATFORM,
    display::{Handle, ProgressState},
    error::TsdlError,
    logging, TsdlResult,
};

fn main() -> ExitCode {
    set_panic_hook();
    let args = args::Args::parse();

    if let Err(e) = logging::init(&args) {
        eprintln!("Could not initialize logging: {e}");
        ExitCode::FAILURE
    } else {
        info!("Starting");
        match App::new(&args).and_then(|app| run(&app, &args)) {
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
            Ok(()) => ExitCode::SUCCESS,
        }
    }
}

fn run(app: &App, args: &args::Args) -> TsdlResult<()> {
    match &args.command {
        args::Command::Build(_) => tsdl::build::run(app),
        args::Command::Config { command } => tsdl::config::run(app, command),
        args::Command::Selfupdate => execute_selfupdate(app),
    }
}

fn execute_selfupdate(app: &App) -> TsdlResult<()> {
    let mut progress = app
        .progress
        .lock()
        .map_err(|e| TsdlError::message(format!("Failed to acquire progress lock: {e}")))?;
    let tsdl = env!("CARGO_BIN_NAME");
    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| TsdlError::context("Failed to parse current version", e))?;
    let mut handle = progress.register("selfupdate", 4);

    handle.start("fetching releases".to_string());
    let releases = self_update::backends::github::ReleaseList::configure()
        .repo_owner("stackmystack")
        .repo_name(tsdl)
        .build()
        .map_err(|e| TsdlError::context("Failed to build release list configuration", e))?
        .fetch()
        .map_err(|e| TsdlError::context("Failed to fetch releases", e))?;

    let name = format!("{tsdl}-{TREE_SITTER_PLATFORM}.gz");
    let asset = releases[0].assets.iter().find(|&asset| asset.name == name);
    if asset.is_none() {
        return Err(TsdlError::message(
            "Could not find a suitable release for your platform",
        ));
    }

    let latest_version = Version::parse(&releases[0].version)
        .map_err(|e| TsdlError::context("Failed to parse latest version", e))?;
    if latest_version <= current_version {
        handle.msg("already at the latest version".to_string());
        return Ok(());
    }

    handle.step(format!("downloading {latest_version}"));
    let asset = asset.unwrap();
    let tmp_dir = tempfile::tempdir()
        .map_err(|e| TsdlError::context("Failed to create temporary directory", e))?;
    let tmp_gz_path = tmp_dir.path().join(&asset.name);
    let tmp_gz = fs::File::create_new(&tmp_gz_path)
        .map_err(|e| TsdlError::context("Failed to create temporary file", e))?;

    self_update::Download::from_url(&asset.download_url)
        .set_header(
            reqwest::header::ACCEPT,
            "application/octet-stream"
                .parse()
                .map_err(|e| TsdlError::context("Failed to parse accept header", e))?,
        )
        .download_to(&tmp_gz)
        .map_err(|e| TsdlError::context("Failed to download release asset", e))?;

    handle.step(format!("extracting {latest_version}"));
    let tsdl_bin = PathBuf::from(tsdl);
    self_update::Extract::from_source(&tmp_gz_path)
        .archive(self_update::ArchiveKind::Plain(Some(
            self_update::Compression::Gz,
        )))
        .extract_file(tmp_dir.path(), &tsdl_bin)
        .map_err(|e| TsdlError::context("Failed to extract release asset", e))?;

    let new_exe = tmp_dir.path().join(tsdl_bin);
    self_replace::self_replace(new_exe)
        .map_err(|e| TsdlError::context("Failed to replace current executable", e))?;

    handle.fin(format!("{latest_version}"));
    Ok(())
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
