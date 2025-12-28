use std::{
    ffi::OsStr,
    fmt,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Output, Stdio},
};

use serde::{Deserialize, Serialize};
use tokio::{fs, process::Command};

use crate::{error::TsdlError, sh::Exec, TsdlResult};
use derive_more::{AsRef, Deref};

use std::sync::Arc;

#[derive(AsRef, Clone, Deref, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRef(pub Arc<str>);

impl From<String> for GitRef {
    fn from(s: String) -> Self {
        Self(s.into())
    }
}

impl From<&str> for GitRef {
    fn from(s: &str) -> Self {
        Self(s.into())
    }
}

impl std::str::FromStr for GitRef {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.into()))
    }
}

impl GitRef {
    /// Create a new `GitRef` from a string slice
    #[must_use]
    pub fn new(s: &str) -> Self {
        Self(s.into())
    }

    /// Get as string slice
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum Tag {
    Exact { label: String, sha1: GitRef },
    Ref(GitRef),
}

impl Tag {
    #[must_use]
    pub fn git_ref(&self) -> &GitRef {
        match self {
            Tag::Exact { sha1, .. } => sha1,
            Tag::Ref(r) => r,
        }
    }
}

impl fmt::Display for GitRef {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let git_ref = if self.0.len() == 40 && self.0.chars().all(|c| c.is_ascii_hexdigit()) {
            &self.0[..7]
        } else {
            &self.0
        };
        write!(f, "{git_ref}")
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Tag::Exact { label, .. } => write!(f, "{label}"),
            Tag::Ref(ref_) => write!(f, "{ref_}"),
        }
    }
}

// TODO: get rid of async fs completely.
async fn clean_anyway(cwd: &Path) -> TsdlResult<()> {
    if cwd.exists() {
        if cwd.is_dir() {
            fs::remove_dir_all(cwd).await
        } else {
            fs::remove_file(cwd).await
        }?;
    }
    Ok(())
}

pub async fn clone(repo: &str, cwd: &Path) -> TsdlResult<()> {
    if cwd.exists() {
        Command::new("git")
            .current_dir(cwd)
            .args(["pull"])
            .exec()
            .await?;
    } else {
        Command::new("git")
            .args(["clone", repo, &format!("{}", cwd.display())])
            .exec()
            .await?;
    }
    Ok(())
}

pub async fn clone_fast(repo: &str, git_ref: &str, cwd: &Path) -> TsdlResult<()> {
    clone_fast_with_force(repo, git_ref, cwd, false).await
}

pub async fn clone_fast_with_force(
    repo: &str,
    git_ref: &str,
    cwd: &Path,
    force: bool,
) -> TsdlResult<()> {
    if force || !is_same_remote(cwd, repo).await {
        clean_anyway(cwd).await?;
    }
    if is_valid_git_dir(cwd).await {
        reset_head_hard(cwd, git_ref).await?;
    } else {
        init_fetch_and_checkout(cwd, repo, git_ref).await?;
    }
    Ok(())
}

pub fn column(input: &str, indent: &str, width: usize) -> TsdlResult<Output> {
    let mut child = std::process::Command::new("git")
        .arg("column")
        .arg("--mode=always")
        .arg(format!("--indent={indent}"))
        .arg(format!("--width={width}",))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    let Some(mut stdin) = child.stdin.take() else {
        return child
            .wait_with_output()
            .map_err(|e| TsdlError::context("git column did not finish normally", e));
    };

    stdin
        .write_all(input.as_bytes())
        .map_err(|e| TsdlError::context("Failed to write to git column stdin", e))?;

    child
        .wait_with_output()
        .map_err(|e| TsdlError::context("git column did not finish normally", e))
}

async fn fetch_and_checkout(cwd: &Path, git_ref: &str) -> TsdlResult<()> {
    Command::new("git")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(cwd)
        .args(["fetch", "origin", "--depth", "1", git_ref])
        .exec()
        .await?;
    Command::new("git")
        .current_dir(cwd)
        .args(["reset", "--hard", "FETCH_HEAD"])
        .exec()
        .await?;
    Ok(())
}

async fn get_head_sha1(cwd: &Path) -> TsdlResult<String> {
    String::from_utf8(
        Command::new("git")
            .current_dir(cwd)
            .args(["rev-parse", "HEAD"])
            .exec()
            .await?
            .stdout,
    )
    .map_err(|e| TsdlError::context("rev-parse HEAD is not a valid utf-8", e))
}

async fn get_remote_url(cwd: &Path) -> TsdlResult<String> {
    String::from_utf8(
        Command::new("git")
            .current_dir(cwd)
            .args(["remote", "get-url", "origin"])
            .exec()
            .await?
            .stdout,
    )
    .map_err(|e| TsdlError::context("remote get-url origin did not return a valid utf-8", e))
}

async fn init_fetch_and_checkout(cwd: &Path, repo: &str, git_ref: &str) -> TsdlResult<()> {
    clean_anyway(cwd).await?;
    fs::create_dir_all(cwd).await?;

    Command::new("git")
        .current_dir(cwd)
        .arg("init")
        .exec()
        .await?;

    Command::new("git")
        .current_dir(cwd)
        .args(["remote", "add", "origin", repo])
        .exec()
        .await?;

    fetch_and_checkout(cwd, git_ref).await?;

    Ok(())
}

async fn is_same_remote(cwd: &Path, remote: &str) -> bool {
    remote == get_remote_url(cwd).await.unwrap_or_default().trim()
}

async fn is_valid_git_dir(cwd: &Path) -> bool {
    let is_inside_work_tree = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "--is-inside-work-tree"])
        .exec()
        .await
        .is_ok();
    let can_parse_head = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "HEAD"])
        .exec()
        .await
        .is_ok();

    is_inside_work_tree && can_parse_head
}

pub async fn list_grammar_files(cwd: &Path) -> TsdlResult<Vec<PathBuf>> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .exec()
        .await?;

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| TsdlError::context("git ls-files output is not valid utf-8", e))?;

    let exclude = [
        ".github", "bindings", "doc", "docs", "examples", "queries", "script", "scripts", "test",
        "tests",
    ];

    let result: Vec<PathBuf> = stdout
        .lines()
        .filter_map(|line| {
            if line.is_empty() {
                return None;
            }

            let path = Path::new(line);

            // Check if filename is exactly "grammar.js"
            if path.file_name() != Some(OsStr::new("grammar.js")) {
                return None;
            }

            // Check if any path component is in excluded dirs
            let has_excluded = path.components().any(|comp| {
                if let Component::Normal(name) = comp {
                    exclude.contains(&name.to_string_lossy().as_ref())
                } else {
                    false
                }
            });

            if has_excluded {
                return None;
            }

            Some(PathBuf::from(line))
        })
        .collect();

    Ok(result)
}

async fn reset_head_hard(cwd: &Path, git_ref: &str) -> TsdlResult<()> {
    if git_ref != get_head_sha1(cwd).await?.trim() {
        Command::new("git")
            .current_dir(cwd)
            .args(["reset", "--hard", "HEAD"])
            .exec()
            .await?;
        fetch_and_checkout(cwd, git_ref).await?;
    }
    Ok(())
}

pub async fn tag_for_ref(cwd: &Path, git_ref: &str) -> TsdlResult<String> {
    // Try to find a tag for this ref
    let tag = Command::new("git")
        .current_dir(cwd)
        .args(["describe", "--abbrev=0", "--tags", git_ref])
        .exec()
        .await;

    if let Ok(output) = tag {
        // Found a tag, use it
        String::from_utf8(output.stdout)
            .map_err(|e| TsdlError::context("Failed to parse git tag output as UTF-8", e))
            .map(|s| s.trim().to_string())
    } else {
        // No tag found (e.g., ref is a branch), fall back to commit SHA1
        let sha1 = Command::new("git")
            .current_dir(cwd)
            .args(["rev-parse", git_ref])
            .exec()
            .await?;
        String::from_utf8(sha1.stdout)
            .map_err(|e| TsdlError::context("Failed to parse git rev-parse output as UTF-8", e))
            .map(|s| s.trim().to_string())
    }
}
