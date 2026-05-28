use std::borrow::Cow;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::result::Result as StdResult;

use async_compression::tokio::bufread::GzipDecoder;
use tempfile::TempPath;
use tokio::{fs, io, process::Command};
use tracing::{debug, info, trace, warn};
use url::Url;

use crate::actors;
use crate::args;
use crate::git;
use crate::sh::Exec;
use crate::shutdown;
use crate::{Error, Result, ResultExt, SafeCanonicalize};

#[derive(Debug, Clone)]
pub struct PreparedCli {
    pub path: PathBuf,
    pub tree_sitter: args::TreeSitter,
}

#[derive(Debug, PartialEq, Eq)]
enum CliCacheStatus {
    Hit,
    Missing,
    Invalid(String),
}

async fn chmod_x(prog: &Path) -> Result<()> {
    let metadata = fs::metadata(prog)
        .await
        .with_context(|| format!("getting metadata for {}", prog.display()))?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(prog, permissions)
        .await
        .with_context(|| format!("chmod +x {}", prog.display()))
}

async fn cli(
    build_dir: &Path,
    handle: &actors::ProgressAddr,
    platform: &str,
    repo: &str,
    tag: &str,
) -> Result<PathBuf> {
    let cli = format!("tree-sitter-{platform}");
    let res = build_dir.join(format!("{cli}-{tag}")).canon()?;

    let gz_basename = format!("{cli}.gz");
    let url = format!("{repo}/releases/download/{tag}/{gz_basename}");

    match check_cached_cli(&res, tag).await? {
        CliCacheStatus::Hit => {
            info!("Using cached tree-sitter CLI at {}", res.display());
            handle.set_outcome_cached().await;
            handle.step("cached");
        }
        CliCacheStatus::Missing => {
            info!("Tree-sitter CLI cache miss for {tag}; downloading from {url}");
            handle.set_outcome_built().await;
            handle.step("downloading");
            download_and_install(&url, &res, tag).await?;
        }
        CliCacheStatus::Invalid(reason) => {
            warn!(
                "Cached tree-sitter CLI at {} is invalid ({reason}); re-downloading from {url}",
                res.display()
            );
            handle.msg("cached CLI invalid; re-downloading");
            handle.set_outcome_built().await;
            handle.step("downloading");
            download_and_install(&url, &res, tag).await?;
        }
    }

    Ok(res)
}

async fn resolve_release_tag(
    build_dir: &Path,
    handle: &actors::ProgressAddr,
    repo: &str,
    resolved_ref: &git::ResolvedRef,
) -> Result<String> {
    let tag = match resolved_ref {
        git::ResolvedRef::Tag { label, .. } => Cow::Borrowed(label.as_str()),
        git::ResolvedRef::Ref(git_ref) => {
            handle.msg(format!("resolving exact tag for {resolved_ref}"));
            let tree_sitter = build_dir.join("tree-sitter");
            git::clone(repo, &tree_sitter).await?;
            Cow::Owned(git::tag_for_ref(&tree_sitter, git_ref).await?)
        }
    };
    Ok(tag.into_owned())
}

async fn check_cached_cli(path: &Path, tag: &str) -> Result<CliCacheStatus> {
    let metadata = match fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CliCacheStatus::Missing)
        }
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Inspecting cached tree-sitter CLI {}", path.display()));
        }
    };

    let file_type = metadata.file_type();
    if file_type.is_dir() {
        return Err(Error::Message {
            message: format!(
                "Cached tree-sitter CLI path is a directory and cannot be replaced: {}",
                path.display()
            ),
        });
    }

    if file_type.is_symlink() {
        return Ok(CliCacheStatus::Invalid(
            "cached path is a symbolic link".to_string(),
        ));
    }

    if !file_type.is_file() {
        return Ok(CliCacheStatus::Invalid(
            "cached path is not a regular file".to_string(),
        ));
    }

    if metadata.permissions().mode() & 0o111 == 0 {
        return Ok(CliCacheStatus::Invalid(
            "cached file is not executable".to_string(),
        ));
    }

    match verify_cli(path, tag).await {
        Ok(()) => Ok(CliCacheStatus::Hit),
        Err(Error::Interrupted { signal }) => Err(Error::Interrupted { signal }),
        Err(err) => Ok(CliCacheStatus::Invalid(first_line(&err.to_string()))),
    }
}

async fn download(url: &str, gz: &Path) -> Result<()> {
    let response = reqwest::get(url)
        .await
        .with_context(|| format!("Fetching tree-sitter CLI from {url}"))?
        .error_for_status()
        .with_context(|| format!("Downloading tree-sitter CLI from {url}"))?;

    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("Reading tree-sitter CLI response body from {url}"))?;

    fs::write(gz, bytes)
        .await
        .with_context(|| format!("Writing tree-sitter CLI archive to {}", gz.display()))
}

async fn download_and_install(url: &str, res: &Path, tag: &str) -> Result<()> {
    let gz = temp_path_for(res, ".gz")?;
    let cli = temp_path_for(res, ".bin")?;

    info!("Downloading tree-sitter CLI from {url}");
    download(url, gz.as_ref()).await?;
    install_downloaded_cli(gz.as_ref(), cli.as_ref(), res, tag).await?;
    info!("Installed tree-sitter CLI at {}", res.display());
    Ok(())
}

async fn install_downloaded_cli(gz: &Path, tmp_cli: &Path, res: &Path, tag: &str) -> Result<()> {
    gunzip(gz, tmp_cli).await?;
    chmod_x(tmp_cli).await?;
    context_unless_interrupted(verify_cli(tmp_cli, tag).await, || {
        format!("Verifying downloaded tree-sitter CLI {}", tmp_cli.display())
    })?;
    promote_cli(tmp_cli, res).await
}

fn temp_path_for(res: &Path, suffix: &str) -> Result<TempPath> {
    let parent = res.parent().ok_or_else(|| Error::Message {
        message: format!(
            "Could not determine parent directory for tree-sitter CLI path {}",
            res.display()
        ),
    })?;
    let name = res.file_name().map_or_else(
        || Cow::Borrowed("tree-sitter"),
        |name| name.to_string_lossy(),
    );

    tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(suffix)
        .tempfile_in(parent)
        .map(tempfile::NamedTempFile::into_temp_path)
        .with_context(|| {
            format!(
                "Creating temporary tree-sitter CLI file in {}",
                parent.display()
            )
        })
}

async fn promote_cli(tmp_cli: &Path, res: &Path) -> Result<()> {
    if let Err(err) = fs::rename(tmp_cli, res).await {
        let _ = fs::remove_file(tmp_cli).await;
        return Err(err).with_context(|| {
            format!(
                "Installing tree-sitter CLI {} to {}",
                tmp_cli.display(),
                res.display()
            )
        });
    }

    Ok(())
}

async fn verify_cli(path: &Path, tag: &str) -> Result<()> {
    let output =
        context_unless_interrupted(Command::new(path).arg("--version").exec().await, || {
            format!("Running {} --version", path.display())
        })?;

    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    if let Some(expected) = expected_cli_version(tag) {
        if !output.contains(expected) {
            return Err(Error::Message {
                message: format!(
                    "tree-sitter CLI version output did not contain expected version {expected:?}: {}",
                    output.trim()
                ),
            });
        }
    }

    Ok(())
}

fn expected_cli_version(tag: &str) -> Option<&str> {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    is_dotted_numeric_version(version).then_some(version)
}

fn first_line(message: &str) -> String {
    message
        .lines()
        .next()
        .unwrap_or("unknown verification failure")
        .to_string()
}

fn context_unless_interrupted<T>(result: Result<T>, message: impl FnOnce() -> String) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(err @ Error::Interrupted { .. }) => Err(err),
        Err(err) => Err(Error::Context {
            message: message(),
            source: err.into(),
        }),
    }
}

fn find_tag(
    refs: &HashMap<String, String>,
    version: &str,
) -> StdResult<git::ResolvedRef, crate::git::RefError> {
    refs.get_key_value(&format!("v{version}"))
        .or_else(|| refs.get_key_value(version))
        .map_or_else(
            || git::Ref::new(normalize_release_ref(version)).map(git::ResolvedRef::Ref),
            |(k, v)| {
                trace!("Found! {k} -> {v}");
                Ok(git::ResolvedRef::Tag {
                    sha: git::Sha::new(v.as_str())?,
                    label: k.clone(),
                })
            },
        )
}

fn is_dotted_numeric_version(value: &str) -> bool {
    !value.is_empty()
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.parse::<u32>().is_ok())
}

fn normalize_release_ref(value: &str) -> String {
    if git::Sha::is_full_sha(value) || value.starts_with('v') {
        value.to_string()
    } else if is_dotted_numeric_version(value) {
        format!("v{value}")
    } else {
        value.to_string()
    }
}

async fn gunzip(gz: &Path, to: &Path) -> Result<()> {
    let file = fs::File::open(gz)
        .await
        .with_context(|| format!("opening {}", gz.display()))?;
    let mut decompressor = GzipDecoder::new(tokio::io::BufReader::new(file));

    let mut file = tokio::fs::File::create(to)
        .await
        .with_context(|| format!("creating {}", to.display()))?;

    io::copy(&mut decompressor, &mut file)
        .await
        .map(|_| ())
        .with_context(|| format!("decompressing {}", gz.display()))?;
    file.sync_all()
        .await
        .with_context(|| format!("syncing extracted tree-sitter CLI {}", to.display()))
}

fn parse_refs(stdout: &str) -> HashMap<String, String> {
    let mut refs = HashMap::new();

    for line in stdout.lines() {
        let ref_line = line.split('\t').map(str::trim).collect::<Vec<_>>();
        let (sha1, full_ref) = (ref_line[0], ref_line[1]);
        let Some(tag) = full_ref.strip_prefix("refs/tags/") else {
            continue;
        };
        trace!("insert {tag} -> {sha1}");
        refs.insert(tag.to_string(), sha1.to_string());
    }

    refs
}

pub async fn prepare(
    build_dir: &Path,
    display: actors::DisplayAddr,
    tree_sitter: &args::TreeSitter,
) -> Result<PreparedCli> {
    shutdown::test_delay().await;
    shutdown::check()?;
    debug!("[prepare] tree-sitter-cli version={}", tree_sitter.version);

    let progress = display
        .add_language(
            display_tree_sitter_ref(&tree_sitter.version)?,
            "tree-sitter-cli",
            2,
        )
        .await;

    let repo = Url::parse(&tree_sitter.repo).context("Parsing the tree-sitter URL")?;
    let git_ref = &tree_sitter.version;

    progress.step(format!("resolving {git_ref}"));
    let tag = match tag(repo.as_str(), git_ref).await {
        Ok(tag) => tag,
        Err(e) => {
            if shutdown::current().is_some_and(|s| s.is_cancelled()) {
                progress.cancel().await;
            } else {
                progress.err("resolve failed").await;
            }
            return Err(e);
        }
    };
    let release_tag = match resolve_release_tag(build_dir, &progress, &tree_sitter.repo, &tag).await
    {
        Ok(tag) => tag,
        Err(e) => {
            if shutdown::current().is_some_and(|s| s.is_cancelled()) {
                progress.cancel().await;
            } else {
                progress.err("resolve failed").await;
            }
            return Err(e);
        }
    };
    info!("Resolved tree-sitter CLI ref {git_ref:?} to release tag {release_tag:?}");

    let cli = match context_unless_interrupted(
        cli(
            build_dir,
            &progress,
            &tree_sitter.platform,
            &tree_sitter.repo,
            &release_tag,
        )
        .await,
        || {
            format!(
                "Preparing tree-sitter CLI {release_tag} for platform {}",
                tree_sitter.platform
            )
        },
    ) {
        Ok(cli) => cli,
        Err(e) => {
            if shutdown::current().is_some_and(|s| s.is_cancelled()) {
                progress.cancel().await;
            } else {
                progress.err("download failed").await;
            }
            return Err(e);
        }
    };
    progress.fin("done").await;

    Ok(PreparedCli {
        path: cli,
        tree_sitter: args::TreeSitter {
            version: release_tag,
            platform: tree_sitter.platform.clone(),
            repo: tree_sitter.repo.clone(),
        },
    })
}

pub(crate) fn display_tree_sitter_ref(version: &str) -> StdResult<git::Ref, crate::git::RefError> {
    git::Ref::new(normalize_release_ref(version))
}

#[allow(clippy::missing_panics_doc)]
pub async fn tag(repo: &str, version: &str) -> Result<git::ResolvedRef> {
    let output = Command::new("git")
        .args(["ls-remote", "--refs", "--tags", repo])
        .exec()
        .await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let refs = parse_refs(&stdout);
    find_tag(&refs, version).with_context(|| format!("Parsing tree-sitter git ref {version:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as std_fs;

    fn write_script(path: &Path, body: &str) {
        std_fs::write(path, body).unwrap();
        let mut permissions = std_fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std_fs::set_permissions(path, permissions).unwrap();
    }

    #[tokio::test]
    async fn cached_cli_missing_is_miss() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tree-sitter-test-v1.2.3");

        assert_eq!(
            check_cached_cli(&path, "v1.2.3").await.unwrap(),
            CliCacheStatus::Missing
        );
    }

    #[tokio::test]
    async fn cached_cli_non_executable_is_invalid() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tree-sitter-test-v1.2.3");
        std_fs::write(&path, b"tree-sitter 1.2.3\n").unwrap();

        match check_cached_cli(&path, "v1.2.3").await.unwrap() {
            CliCacheStatus::Invalid(reason) => assert!(reason.contains("not executable")),
            status => panic!("expected invalid cache entry, got {status:?}"),
        }
    }

    #[tokio::test]
    async fn cached_cli_valid_version_hits() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tree-sitter-test-v1.2.3");
        write_script(&path, "#!/bin/sh\necho 'tree-sitter 1.2.3'\n");

        assert_eq!(
            check_cached_cli(&path, "v1.2.3").await.unwrap(),
            CliCacheStatus::Hit
        );
    }

    #[tokio::test]
    async fn cached_cli_wrong_version_is_invalid() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tree-sitter-test-v1.2.3");
        write_script(&path, "#!/bin/sh\necho 'tree-sitter 9.9.9'\n");

        match check_cached_cli(&path, "v1.2.3").await.unwrap() {
            CliCacheStatus::Invalid(reason) => assert!(reason.contains("expected version")),
            status => panic!("expected invalid cache entry, got {status:?}"),
        }
    }

    #[tokio::test]
    async fn failed_extraction_does_not_create_final_cli_path() {
        let temp = tempfile::tempdir().unwrap();
        let gz = temp.path().join("tree-sitter-test.gz");
        let tmp_cli = temp.path().join("tree-sitter-test.tmp");
        let final_cli = temp.path().join("tree-sitter-test");
        std_fs::write(&gz, b"not a gzip archive").unwrap();

        let err = install_downloaded_cli(&gz, &tmp_cli, &final_cli, "v1.2.3")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("decompressing"));
        assert!(!final_cli.exists());
    }

    #[test]
    fn test_parse_refs_empty() {
        let stdout = "";
        let refs = parse_refs(stdout);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_parse_refs() {
        let stdout = "abc123\trefs/tags/v1.0.0\nuwu456\trefs/tags/release\nxyz789\trefs/tags/v2.0.0\nbranch\trefs/heads/main\nslash\trefs/tags/channel/release";
        let refs = parse_refs(stdout);
        assert_eq!(refs.get("v1.0.0"), Some(&"abc123".to_string()));
        assert_eq!(refs.get("release"), Some(&"uwu456".to_string()));
        assert_eq!(refs.get("v2.0.0"), Some(&"xyz789".to_string()));
        assert_eq!(refs.get("channel/release"), Some(&"slash".to_string()));
        assert_eq!(refs.get("main"), None);
    }

    #[test]
    fn test_find_tag_exact() {
        let mut refs = HashMap::new();
        refs.insert(
            "v1.0.0".to_string(),
            "636801770eea172d140e64b691815ff11f6b556f".to_string(),
        );
        let tag = find_tag(&refs, "1.0.0").unwrap();
        match tag {
            git::ResolvedRef::Tag { sha, label } => {
                assert_eq!(sha.as_str(), "636801770eea172d140e64b691815ff11f6b556f");
                assert_eq!(label, "v1.0.0");
            }
            git::ResolvedRef::Ref(_) => panic!("Expected git::ResolvedRef::Tag"),
        }
    }

    #[test]
    fn test_find_tag_ref() {
        let refs = HashMap::new();
        let tag = find_tag(&refs, "1.0.0").unwrap();
        match tag {
            git::ResolvedRef::Ref(git_ref) => {
                assert_eq!(git_ref.as_str(), "v1.0.0");
            }
            git::ResolvedRef::Tag { .. } => panic!("Expected git::ResolvedRef::Ref"),
        }
    }
}
