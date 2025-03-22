use std::{fs, path::PathBuf};

use anyhow::{bail, Result};
use clap::Parser;
use self_update::self_replace;
use semver::Version;
use tracing::{error, info};

use tsdl::{
    args, build, config,
    consts::TREE_SITTER_PLATFORM,
    display::{self, Handle, Progress, ProgressState},
    logging,
};

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
            display::current(&args.progress, &args.verbose),
        ),
        args::Command::Config { command } => config::run(command, &args.config),
        args::Command::Selfupdate => self_update(display::current(&args.progress, &args.verbose)),
    }
}

fn self_update(mut progress: Progress) -> Result<()> {
    let tsdl = env!("CARGO_BIN_NAME");
    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let mut handle = progress.register("selfupdate", 4);

    handle.start("fetching releases".to_string());
    let releases = self_update::backends::github::ReleaseList::configure()
        .repo_owner("stackmystack")
        .repo_name(tsdl)
        .build()?
        .fetch()?;

    let name = format!("{tsdl}-{TREE_SITTER_PLATFORM}.gz");
    let asset = releases[0].assets.iter().find(|&asset| asset.name == name);
    if asset.is_none() {
        bail!("Could not find a suitable release for your platform");
    }

    let latest_version = Version::parse(&releases[0].version)?;
    if latest_version <= current_version {
        handle.msg("already at the latest version".to_string());
        return Ok(());
    }

    handle.step(format!("downloading {latest_version}"));
    let asset = asset.unwrap();
    let tmp_dir = tempfile::tempdir()?;
    let tmp_gz_path = tmp_dir.path().join(&asset.name);
    let tmp_gz = fs::File::create_new(&tmp_gz_path)?;

    self_update::Download::from_url(&asset.download_url)
        .set_header(reqwest::header::ACCEPT, "application/octet-stream".parse()?)
        .download_to(&tmp_gz)?;

    handle.step(format!("extracting {latest_version}"));
    let tsdl_bin = PathBuf::from(tsdl);
    self_update::Extract::from_source(&tmp_gz_path)
        .archive(self_update::ArchiveKind::Plain(Some(
            self_update::Compression::Gz,
        )))
        .extract_file(tmp_dir.path(), &tsdl_bin)?;

    let new_exe = tmp_dir.path().join(tsdl_bin);
    self_replace::self_replace(new_exe)?;

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
