use std::borrow::Cow;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use async_compression::tokio::bufread::GzipDecoder;
use tokio::{fs, io, process::Command};
use tracing::trace;
use url::Url;

use crate::actors::{DisplayAddr, ProgressAddr};
use crate::args::TreeSitter;
use crate::consts::TREE_SITTER_REF;
use crate::git::{self, GitRef};
use crate::SafeCanonicalize;
use crate::{error::TsdlError, TsdlResult};
use crate::{git::Tag, sh::Exec};

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
    tag: &Tag,
    tree_sitter: &TreeSitter,
) -> TsdlResult<PathBuf> {
    let platform = &tree_sitter.platform;
    let repo = &tree_sitter.repo;
    let tag = match tag {
        Tag::Exact { label, .. } => Cow::Borrowed(label),
        Tag::Ref(git_ref) => {
            handle.msg(format!("Figuring out the exact tag for {tag}",));
            let tree_sitter = PathBuf::new().join(build_dir).join("tree-sitter");
            git::clone(repo, &tree_sitter).await?;
            Cow::Owned(git::tag_for_ref(&tree_sitter, git_ref).await?)
        }
    };
    let cli = format!("tree-sitter-{platform}");
    let res = PathBuf::new().join(build_dir).join(&cli).canon()?;

    if !res.exists() {
        handle.msg(format!("Downloading {tag}",));
        let gz_basename = format!("{cli}.gz");
        let url = format!("{repo}/releases/download/{tag}/{gz_basename}");
        let gz = PathBuf::new().join(build_dir).join(gz_basename);

        download_and_extract(&gz, &url, &res).await?;
    }

    Ok(res)
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
    gunzip(gz).await?;
    chmod_x(res).await?;
    fs::remove_file(gz)
        .await
        .map_err(|e| TsdlError::context(format!("removing {}", gz.display()), e))?;
    Ok(())
}

fn find_tag(refs: &HashMap<String, String>, version: &str) -> Tag {
    refs.get_key_value(&format!("v{version}"))
        .or_else(|| refs.get_key_value(version))
        .map_or_else(
            || Tag::Ref(GitRef::from_str(version).unwrap()),
            |(k, v)| {
                trace!("Found! {k} -> {v}");
                Tag::Exact {
                    sha1: GitRef::from_str(v).unwrap(),
                    label: k.clone(),
                }
            },
        )
}

async fn gunzip(gz: &Path) -> TsdlResult<()> {
    let file = fs::File::open(gz)
        .await
        .map_err(|e| TsdlError::context(format!("opening {}", gz.display()), e))?;
    let mut decompressor = GzipDecoder::new(tokio::io::BufReader::new(file));
    let path = gz.with_extension("");

    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|e| TsdlError::context(format!("creating {}", path.display()), e))?;

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
        let Some(tag) = full_ref.split('/').next_back() else {
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
) -> TsdlResult<PathBuf> {
    let progress = display
        .add_language(
            "Preparing tree-sitter-cli".into(),
            format!("v{TREE_SITTER_REF}").into(),
            3,
        )
        .await?;

    let repo = Url::parse(&tree_sitter.repo)
        .map_err(|e| TsdlError::context("Parsing the tree-sitter URL", e))?;
    let git_ref = &tree_sitter.git_ref;

    progress.step(format!("Figuring out tag from ref {git_ref}"));
    let tag = tag(repo.as_str(), git_ref).await?;

    progress.step(format!("Fetching {tag}",));
    let cli = cli(build_dir, &progress, &tag, tree_sitter).await?;
    progress.fin(format!("{tag}"));

    Ok(cli)
}
#[allow(clippy::missing_panics_doc)]
pub async fn tag(repo: &str, version: &str) -> TsdlResult<Tag> {
    let output = Command::new("git")
        .args(["ls-remote", "--refs", "--tags", repo])
        .exec()
        .await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let refs = parse_refs(&stdout);
    Ok(find_tag(&refs, version))
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
        let stdout =
            "abc123\trefs/tags/v1.0.0\nuwu456\trefs/tags/release\nxyz789\trefs/tags/v2.0.0";
        let refs = parse_refs(stdout);
        assert_eq!(refs.get("v1.0.0"), Some(&"abc123".to_string()));
        assert_eq!(refs.get("release"), Some(&"uwu456".to_string()));
        assert_eq!(refs.get("v2.0.0"), Some(&"xyz789".to_string()));
    }

    #[test]
    fn test_find_tag_exact() {
        let mut refs = HashMap::new();
        refs.insert("v1.0.0".to_string(), "abc123".to_string());
        let tag = find_tag(&refs, "1.0.0");
        match tag {
            Tag::Exact { sha1, label } => {
                assert_eq!(sha1.as_str(), "abc123");
                assert_eq!(label, "v1.0.0");
            }
            Tag::Ref(_) => panic!("Expected Tag::Exact"),
        }
    }

    #[test]
    fn test_find_tag_ref() {
        let refs = HashMap::new();
        let tag = find_tag(&refs, "1.0.0");
        match tag {
            Tag::Ref(git_ref) => {
                assert_eq!(git_ref.as_str(), "1.0.0");
            }
            Tag::Exact { .. } => panic!("Expected Tag::Ref"),
        }
    }
}
