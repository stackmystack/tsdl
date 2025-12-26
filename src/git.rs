use std::{
    fmt,
    io::Write,
    path::Path,
    process::{Output, Stdio},
};

use derive_more::{AsRef, Deref, From, FromStr, Into};
use tokio::{fs, process::Command};

use crate::{error::TsdlError, sh::Exec, TsdlResult};

#[derive(AsRef, Clone, Debug, Deref, From, FromStr, Hash, Into, PartialEq, Eq)]
#[as_ref(str, [u8], String)]
pub struct Ref(pub String);

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum Tag {
    Exact { label: String, sha1: Ref },
    Ref(Ref),
}

impl Tag {
    #[must_use]
    pub fn git_ref(&self) -> &Ref {
        match self {
            Tag::Exact { sha1, .. } => sha1,
            Tag::Ref(r) => r,
        }
    }
}

impl fmt::Display for Ref {
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
    if !is_same_remote(cwd, repo).await {
        clean_anyway(cwd).await?;
    }
    if is_valid_git_dir(cwd).await {
        reset_head_hard(cwd, git_ref).await?;
    } else {
        init_fetch_and_checkout(cwd, repo, git_ref).await?;
    }
    Ok(())
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

async fn is_same_remote(cwd: &Path, remote: &str) -> bool {
    remote == get_remote_url(cwd).await.unwrap_or_default().trim()
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

pub async fn tag_for_ref(cwd: &Path, git_ref: &str) -> TsdlResult<String> {
    // Try to find a tag for this ref
    let tag_result = Command::new("git")
        .current_dir(cwd)
        .args(["describe", "--abbrev=0", "--tags", git_ref])
        .exec()
        .await;

    if let Ok(output) = tag_result {
        // Found a tag, use it
        String::from_utf8(output.stdout)
            .map_err(|e| TsdlError::context("Failed to parse git tag output as UTF-8", e))
            .map(|s| s.trim().to_string())
    } else {
        // No tag found (e.g., ref is a branch), fall back to commit SHA1
        let sha1_output = Command::new("git")
            .current_dir(cwd)
            .args(["rev-parse", git_ref])
            .exec()
            .await?;
        String::from_utf8(sha1_output.stdout)
            .map_err(|e| TsdlError::context("Failed to parse git rev-parse output as UTF-8", e))
            .map(|s| s.trim().to_string())
    }
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
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|e| TsdlError::context("Failed to write to git column stdin", e))?;
    }
    child
        .wait_with_output()
        .map_err(|e| TsdlError::context("git column did not finish normally", e))
}
