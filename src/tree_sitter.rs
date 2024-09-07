use std::borrow::Cow;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use async_compression::tokio::bufread::GzipDecoder;
use miette::{miette, Context, IntoDiagnostic, Result};
use tokio::process::Command;
use tokio::{fs, io};
use tracing::trace;
use url::Url;

use crate::display::ProgressHandle;
use crate::git::{self, Ref};
use crate::SafeCanonicalize;
use crate::{
    args::BuildCommand,
    display::{Handle, Progress, ProgressState},
    git::Tag,
    sh::Exec,
};

#[allow(clippy::missing_panics_doc)]
pub async fn tag(repo: &str, version: &str) -> Result<Tag> {
    let output = Command::new("git")
        .args(["ls-remote", "--refs", "--tags", repo])
        .exec()
        .await?;
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let mut refs = HashMap::new();
    for line in stdout.lines() {
        let ref_line = line.split('\t').map(str::trim).collect::<Vec<_>>();
        let (sha1, full_ref) = (ref_line[0], ref_line[1]);
        if let Some(tag) = full_ref.split('/').last() {
            trace!("insert {tag} -> {sha1}");
            refs.insert(tag.to_string(), sha1.to_string());
        }
    }
    Ok(refs
        .get_key_value(&format!("v{version}"))
        .or_else(|| refs.get_key_value(version))
        .map_or_else(
            || Tag::Ref(Ref::from_str(version).unwrap()),
            |(k, v)| {
                trace!("Found! {k} -> {v}");
                Tag::Exact {
                    sha1: Ref::from_str(v).unwrap(),
                    label: k.to_string(),
                }
            },
        ))
}

async fn cli(args: &BuildCommand, tag: &Tag, handle: &ProgressHandle) -> Result<PathBuf> {
    let build_dir = &args.build_dir;
    let platform = &args.tree_sitter.platform;
    let repo = &args.tree_sitter.repo;
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

        download(&gz, &url).await?;
        gunzip(&gz).await?;
        chmod_x(&res).await?;
    }
    Ok(res)
}

async fn download(gz: &Path, url: &str) -> Result<()> {
    fs::write(
        gz,
        reqwest::get(url)
            .await
            .into_diagnostic()?
            .bytes()
            .await
            .into_diagnostic()?,
    )
    .await
    .into_diagnostic()
}

async fn gunzip(gz: &Path) -> Result<()> {
    let file = fs::File::open(gz).await.into_diagnostic()?;
    let mut decompressor = GzipDecoder::new(tokio::io::BufReader::new(file));
    let out_path = gz.with_extension("");
    let mut out_file = tokio::fs::File::create(out_path).await.into_diagnostic()?;
    io::copy(&mut decompressor, &mut out_file)
        .await
        .into_diagnostic()
        .and(Ok(()))
}

async fn chmod_x(prog: &Path) -> Result<()> {
    let metadata = fs::metadata(prog).await.into_diagnostic()?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(prog, permissions)
        .await
        .into_diagnostic()
}

pub async fn prepare(args: &BuildCommand, progress: Arc<Mutex<Progress>>) -> Result<PathBuf> {
    let mut handle = {
        progress
            .lock()
            .map(|mut lock| lock.register("tree-sitter-cli", 3))
            .or(Err(miette!("Acquiring progress lock")))?
    };

    let repo = Url::parse(&args.tree_sitter.repo)
        .into_diagnostic()
        .wrap_err("Parsing the tree-sitter URL")?;
    let version = &args.tree_sitter.version;
    handle.start(format!("Figuring out tag from version {version}"));
    let tag = tag(repo.as_str(), version).await?;
    handle.step(format!("Fetching {tag}",));
    let cli = cli(args, &tag, &handle).await?;
    handle.fin(format!("{tag}"));
    Ok(cli)
}
