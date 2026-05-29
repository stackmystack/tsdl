use std::{fs, path::PathBuf};

use self_update::self_replace;
use semver::Version;

use crate::{Error, Result, ResultExt, args::VersionBump, consts::PLATFORM, prompt_user};

enum UpdateTarget {
  Exact(Version),
  Relative(VersionBump),
}

fn download_and_replace(asset_name: &str, download_url: &str, version: &Version) -> Result<()> {
  let tsdl = env!("CARGO_PKG_NAME");
  let tmp_dir = tempfile::tempdir().context("Failed to create temporary directory")?;
  let tmp_gz_path = tmp_dir.path().join(asset_name);
  let tmp_gz = fs::File::create_new(&tmp_gz_path).context("Failed to create temporary file")?;

  eprintln!("downloading {version}");
  self_update::Download::from_url(download_url)
    .set_header(
      reqwest::header::ACCEPT,
      "application/octet-stream"
        .parse()
        .context("Failed to parse accept header")?,
    )
    .download_to(&tmp_gz)
    .context("Failed to download release asset")?;

  eprintln!("extracting {version}");
  let tsdl_bin = PathBuf::from(tsdl);
  self_update::Extract::from_source(&tmp_gz_path)
    .archive(self_update::ArchiveKind::Plain(Some(
      self_update::Compression::Gz,
    )))
    .extract_file(tmp_dir.path(), &tsdl_bin)
    .with_context(|| "Failed to extract release asset".to_string())?;

  let new_exe = tmp_dir.path().join(tsdl_bin);
  self_replace::self_replace(new_exe)
    .with_context(|| "Failed to replace current executable".to_string())?;

  eprintln!("{version}");
  Ok(())
}

fn parse_target(raw: &str) -> Result<UpdateTarget> {
  match raw {
    | "major" => Ok(UpdateTarget::Relative(VersionBump::Major)),
    | "minor" => Ok(UpdateTarget::Relative(VersionBump::Minor)),
    | "patch" => Ok(UpdateTarget::Relative(VersionBump::Patch)),
    | other => Version::parse(other)
      .map(UpdateTarget::Exact)
      .context("expected 'patch', 'minor', 'major', or a semver like '2.5.0'"),
  }
}

pub fn run(force: bool, target: &str) -> Result<()> {
  let update_target = parse_target(target)?;

  let tsdl = env!("CARGO_PKG_NAME");
  let current_version = Version::parse(env!("CARGO_PKG_VERSION"))
    .with_context(|| "Failed to parse current version".to_string())?;

  eprintln!("fetching releases");
  let releases = self_update::backends::github::ReleaseList::configure()
    .repo_owner("stackmystack")
    .repo_name(tsdl)
    .build()
    .with_context(|| "Failed to build release list configuration".to_string())?
    .fetch()
    .with_context(|| "Failed to fetch releases".to_string())?;

  if releases.is_empty() {
    return Err(Error::Message {
      message: "No releases found".to_string(),
    });
  }

  let (release, version) = match update_target {
    | UpdateTarget::Exact(target_version) => {
      let Some(found_release) = releases
        .iter()
        .find(|r| Version::parse(&r.version).is_ok_and(|v| v == target_version))
      else {
        return Err(Error::Message {
          message: format!("version {target_version} not found in releases"),
        });
      };

      if target_version == current_version {
        eprintln!("already at {target_version}");
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
        eprintln!("downgrade cancelled");
        return Ok(());
      }

      (found_release.clone(), target_version)
    }

    | UpdateTarget::Relative(bump) => {
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
            | VersionBump::Major => true,
            | VersionBump::Minor => rel_ver.major == current_version.major,
            | VersionBump::Patch => {
              rel_ver.major == current_version.major && rel_ver.minor == current_version.minor
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
          eprintln!(
            "no compatible {bump} update (latest is {}; use `tsdl selfupdate major` to install it)",
            releases[0].version,
          );
        } else {
          eprintln!("already at the latest version");
        }
        return Ok(());
      }

      let latest_release = compatible[0];
      let latest_version =
        Version::parse(&latest_release.version).context("Failed to parse latest version")?;
      (latest_release.clone(), latest_version)
    }
  };

  let asset_name = format!("{tsdl}-{PLATFORM}.gz");
  let Some(asset) = release.assets.iter().find(|a| a.name == asset_name) else {
    return Err(Error::Message {
      message: "Could not find a suitable release for your platform".to_string(),
    });
  };

  download_and_replace(&asset.name, &asset.download_url, &version)
}
