use std::{fs, path::PathBuf};

use self_update::self_replace;
use semver::Version;

use crate::{
    app::App, args::VersionBump, consts::TREE_SITTER_PLATFORM, error::TsdlError, prompt_user,
    TsdlResult,
};

enum UpdateTarget {
    Exact(Version),
    Relative(VersionBump),
}

fn download_and_replace(
    asset_name: &str,
    download_url: &str,
    handle: &crate::display::ProgressBar,
    version: &Version,
) -> TsdlResult<()> {
    let tsdl = env!("CARGO_PKG_NAME");
    let tmp_dir = tempfile::tempdir()
        .map_err(|e| TsdlError::context("Failed to create temporary directory", e))?;
    let tmp_gz_path = tmp_dir.path().join(asset_name);
    let tmp_gz = fs::File::create_new(&tmp_gz_path)
        .map_err(|e| TsdlError::context("Failed to create temporary file", e))?;

    handle.step(format!("downloading {version}"));
    self_update::Download::from_url(download_url)
        .set_header(
            reqwest::header::ACCEPT,
            "application/octet-stream"
                .parse()
                .map_err(|e| TsdlError::context("Failed to parse accept header", e))?,
        )
        .download_to(&tmp_gz)
        .map_err(|e| TsdlError::context("Failed to download release asset", e))?;

    handle.step(format!("extracting {version}"));
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

    handle.fin(format!("{version}"));
    Ok(())
}

fn parse_target(raw: &str) -> Result<UpdateTarget, String> {
    match raw {
        "major" => Ok(UpdateTarget::Relative(VersionBump::Major)),
        "minor" => Ok(UpdateTarget::Relative(VersionBump::Minor)),
        "patch" => Ok(UpdateTarget::Relative(VersionBump::Patch)),
        other => Version::parse(other).map(UpdateTarget::Exact).map_err(|e| {
            format!("expected 'patch', 'minor', 'major', or a semver like '2.5.0': {e}")
        }),
    }
}

pub fn run(app: &mut App, force: bool, target: &str) -> TsdlResult<()> {
    let update_target = parse_target(target).map_err(TsdlError::message)?;

    let tsdl = env!("CARGO_PKG_NAME");
    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| TsdlError::context("Failed to parse current version", e))?;
    let handle = app.progress.register("selfupdate".into(), "".into(), 4);

    handle.step("fetching releases");
    let releases = self_update::backends::github::ReleaseList::configure()
        .repo_owner("stackmystack")
        .repo_name(tsdl)
        .build()
        .map_err(|e| TsdlError::context("Failed to build release list configuration", e))?
        .fetch()
        .map_err(|e| TsdlError::context("Failed to fetch releases", e))?;

    if releases.is_empty() {
        return Err(TsdlError::message("No releases found"));
    }

    let (release, version) = match update_target {
        UpdateTarget::Exact(target_version) => {
            let Some(found_release) = releases
                .iter()
                .find(|r| Version::parse(&r.version).is_ok_and(|v| v == target_version))
            else {
                return Err(TsdlError::message(format!(
                    "version {target_version} not found in releases"
                )));
            };

            if target_version == current_version {
                handle.msg(format!("already at {target_version}"));
                return Ok(());
            }

            let is_downgrade = target_version < current_version;
            if is_downgrade
                && !force
                && !prompt_user(
                    &format!("Downgrade from {current_version} to {target_version}?"),
                    false,
                )?
            {
                handle.msg("downgrade cancelled");
                return Ok(());
            }

            (found_release.clone(), target_version)
        }

        UpdateTarget::Relative(bump) => {
            let compatible: Vec<_> = releases
                .iter()
                .filter(|r| {
                    let Ok(rel_ver) = Version::parse(&r.version) else {
                        return false;
                    };
                    if rel_ver <= current_version {
                        return false;
                    }
                    match bump {
                        VersionBump::Major => true,
                        VersionBump::Minor => rel_ver.major == current_version.major,
                        VersionBump::Patch => {
                            rel_ver.major == current_version.major
                                && rel_ver.minor == current_version.minor
                        }
                    }
                })
                .collect();

            if compatible.is_empty() {
                let overall_latest = Version::parse(&releases[0].version).ok();
                if overall_latest
                    .as_ref()
                    .is_some_and(|v| v > &current_version)
                {
                    handle.msg(format!(
                        "no compatible {bump} update (latest is {}; use `tsdl selfupdate major` to install it)",
                        releases[0].version,
                    ));
                } else {
                    handle.msg("already at the latest version");
                }
                return Ok(());
            }

            let latest_release = compatible[0];
            let latest_version = Version::parse(&latest_release.version)
                .map_err(|e| TsdlError::context("Failed to parse latest version", e))?;
            (latest_release.clone(), latest_version)
        }
    };

    let asset_name = format!("{tsdl}-{TREE_SITTER_PLATFORM}.gz");
    let Some(asset) = release.assets.iter().find(|a| a.name == asset_name) else {
        return Err(TsdlError::message(
            "Could not find a suitable release for your platform",
        ));
    };

    download_and_replace(&asset.name, &asset.download_url, &handle, &version)
}
