use std::borrow::Cow;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use async_compression::tokio::bufread::GzipDecoder;
use tokio::{fs, io, process::Command};
use tracing::{debug, info, trace};
use url::Url;

use crate::actors::{DisplayAddr, ProgressAddr};
use crate::args::TreeSitter;
use crate::git::{self, Ref, ResolvedRef, Sha};
use crate::sh::Exec;
use crate::shutdown;
use crate::SafeCanonicalize;
use crate::{error::TsdlError, TsdlResult};

#[derive(Debug, Clone)]
pub struct PreparedCli {
    pub path: PathBuf,
    pub tree_sitter: TreeSitter,
}

async fn chmod_x(prog: &Path) -> TsdlResult<()> {
    let metadata = fs::metadata(prog)
        .await
        .map_err(|e| TsdlError::context(format!("getting metadata for {}", prog.display()), e))?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(prog, permissions)
        .await
        .map_err(|e| TsdlError::context(format!("chmod +x {}", prog.display()), e))
}

async fn cli(
    build_dir: &PathBuf,
    handle: &ProgressAddr,
    platform: &str,
    repo: &str,
    tag: &str,
) -> TsdlResult<PathBuf> {
    let cli = format!("tree-sitter-{platform}");
    let res = PathBuf::new()
        .join(build_dir)
        .join(format!("{cli}-{tag}"))
        .canon()?;

    if res.exists() {
        handle.set_outcome_cached().await;
        handle.step("cached");
    } else {
        handle.set_outcome_built().await;
        handle.step("downloading");
        let gz_basename = format!("{cli}.gz");
        let url = format!("{repo}/releases/download/{tag}/{gz_basename}");
        let gz = PathBuf::new().join(build_dir).join(gz_basename);

        download_and_extract(&gz, &url, &res).await?;
    }

    Ok(res)
}

async fn resolve_release_tag(
    build_dir: &PathBuf,
    handle: &ProgressAddr,
    repo: &str,
    resolved_ref: &ResolvedRef,
) -> TsdlResult<String> {
    let tag = match resolved_ref {
        ResolvedRef::Tag { label, .. } => Cow::Borrowed(label.as_str()),
        ResolvedRef::Ref(git_ref) => {
            handle.msg(format!("resolving exact tag for {resolved_ref}"));
            let tree_sitter = PathBuf::new().join(build_dir).join("tree-sitter");
            git::clone(repo, &tree_sitter).await?;
            Cow::Owned(git::tag_for_ref(&tree_sitter, git_ref).await?)
        }
    };
    Ok(tag.into_owned())
}

async fn download(gz: &Path, url: &str) -> TsdlResult<()> {
    fs::write(
        gz,
        reqwest::get(url)
            .await
            .map_err(|e| TsdlError::context("fetch", e))?
            .bytes()
            .await
            .map_err(|e| TsdlError::context("fetching bytes", e))?,
    )
    .await
    .map_err(|e| TsdlError::context(format!("downloading {url} to {}", gz.display()), e))
}

async fn download_and_extract(gz: &Path, url: &str, res: &Path) -> TsdlResult<()> {
    download(gz, url).await?;
    gunzip(gz, res).await?;
    chmod_x(res).await?;
    fs::remove_file(gz)
        .await
        .map_err(|e| TsdlError::context(format!("removing {}", gz.display()), e))?;
    Ok(())
}

fn find_tag(
    refs: &HashMap<String, String>,
    version: &str,
) -> Result<ResolvedRef, crate::git::RefError> {
    refs.get_key_value(&format!("v{version}"))
        .or_else(|| refs.get_key_value(version))
        .map_or_else(
            || Ref::from_version_or_ref(version).map(ResolvedRef::Ref),
            |(k, v)| {
                trace!("Found! {k} -> {v}");
                Ok(ResolvedRef::Tag {
                    sha: Sha::new(v.as_str())?,
                    label: k.clone(),
                })
            },
        )
}

async fn gunzip(gz: &Path, to: &Path) -> TsdlResult<()> {
    let file = fs::File::open(gz)
        .await
        .map_err(|e| TsdlError::context(format!("opening {}", gz.display()), e))?;
    let mut decompressor = GzipDecoder::new(tokio::io::BufReader::new(file));
    // let path = gz.with_extension("");

    let mut file = tokio::fs::File::create(to)
        .await
        .map_err(|e| TsdlError::context(format!("creating {}", to.display()), e))?;

    io::copy(&mut decompressor, &mut file)
        .await
        .and(Ok(()))
        .map_err(|e| TsdlError::context(format!("decompressing {}", gz.display()), e))
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
    build_dir: &PathBuf,
    display: DisplayAddr,
    tree_sitter: &TreeSitter,
) -> TsdlResult<PreparedCli> {
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

    let repo = Url::parse(&tree_sitter.repo)
        .map_err(|e| TsdlError::context("Parsing the tree-sitter URL", e))?;
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

    let cli = match cli(
        build_dir,
        &progress,
        &tree_sitter.platform,
        &tree_sitter.repo,
        &release_tag,
    )
    .await
    {
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
        tree_sitter: TreeSitter {
            version: release_tag,
            platform: tree_sitter.platform.clone(),
            repo: tree_sitter.repo.clone(),
        },
    })
}

pub(crate) fn display_tree_sitter_ref(version: &str) -> Result<Ref, crate::git::RefError> {
    Ref::from_version_or_ref(version)
}

#[allow(clippy::missing_panics_doc)]
pub async fn tag(repo: &str, version: &str) -> TsdlResult<ResolvedRef> {
    let output = Command::new("git")
        .args(["ls-remote", "--refs", "--tags", repo])
        .exec()
        .await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let refs = parse_refs(&stdout);
    find_tag(&refs, version)
        .map_err(|e| TsdlError::context(format!("Parsing tree-sitter git ref {version:?}"), e))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            ResolvedRef::Tag { sha, label } => {
                assert_eq!(sha.as_str(), "636801770eea172d140e64b691815ff11f6b556f");
                assert_eq!(label, "v1.0.0");
            }
            ResolvedRef::Ref(_) => panic!("Expected ResolvedRef::Tag"),
        }
    }

    #[test]
    fn test_find_tag_ref() {
        let refs = HashMap::new();
        let tag = find_tag(&refs, "1.0.0").unwrap();
        match tag {
            ResolvedRef::Ref(git_ref) => {
                assert_eq!(git_ref.as_str(), "v1.0.0");
            }
            ResolvedRef::Tag { .. } => panic!("Expected ResolvedRef::Ref"),
        }
    }
}
