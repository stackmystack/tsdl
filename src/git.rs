use std::{
    fmt,
    io::Write,
    path::Path,
    process::{Output, Stdio},
};

use derive_more::{AsRef, Deref, From, FromStr, Into};
use miette::{IntoDiagnostic, Result};
use tokio::{fs, process::Command};

use crate::sh::Exec;

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

pub async fn clone(repo: &str, cwd: &Path) -> Result<()> {
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

pub async fn clone_fast(repo: &str, git_ref: &str, cwd: &Path) -> Result<()> {
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

async fn init_fetch_and_checkout(cwd: &Path, repo: &str, git_ref: &str) -> Result<()> {
    clean_anyway(cwd).await?;
    fs::create_dir_all(cwd).await.into_diagnostic()?;

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

async fn reset_head_hard(cwd: &Path, git_ref: &str) -> Result<()> {
    let head_sha1 = String::from_utf8(
        Command::new("git")
            .current_dir(cwd)
            .args(["rev-parse", "HEAD"])
            .exec()
            .await?
            .stdout,
    )
    .into_diagnostic()?;
    if head_sha1.trim() != git_ref {
        Command::new("git")
            .current_dir(cwd)
            .args(["reset", "--hard", "HEAD"])
            .exec()
            .await?;
        fetch_and_checkout(cwd, git_ref).await?;
    }
    Ok(())
}

async fn clean_anyway(cwd: &Path) -> Result<()> {
    if cwd.exists() {
        if cwd.is_dir() {
            fs::remove_dir(cwd).await
        } else {
            fs::remove_file(cwd).await
        }
        .into_diagnostic()?;
    };
    Ok(())
}

async fn is_same_remote(cwd: &Path, remote: &str) -> bool {
    let mut git_remote = Command::new("git");
    git_remote.current_dir(cwd);
    git_remote.args(["remote", "get-url", "origin"]);
    let current_remote = git_remote
        .exec()
        .await
        .map(|f| String::from_utf8(f.stdout).unwrap_or_default())
        .unwrap_or_default();
    current_remote.trim() == remote
}

async fn is_valid_git_dir(cwd: &Path) -> bool {
    let mut git_check = Command::new("git");
    git_check.current_dir(cwd);
    git_check.args(["rev-parse", "--is-inside-work-tree"]);
    let is_inside_work_tree = git_check.exec().await.is_ok();
    let can_parse_head = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "HEAD"])
        .exec()
        .await
        .is_ok();

    is_inside_work_tree && can_parse_head
}

async fn fetch_and_checkout(cwd: &Path, git_ref: &str) -> Result<()> {
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

pub async fn tag_for_ref(cwd: &Path, git_ref: &str) -> Result<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(["describe", "--abbrev=0", "--tags", git_ref])
        .exec()
        .await?;
    Ok(String::from_utf8(output.stdout)
        .into_diagnostic()?
        .trim()
        .to_string())
}

pub fn column(input: &str, indent: &str, width: usize) -> Result<Output> {
    let mut child = std::process::Command::new("git")
        .arg("column")
        .arg("--mode=always")
        .arg(format!("--indent={indent}"))
        .arg(format!("--width={width}",))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .into_diagnostic()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).into_diagnostic()?;
    }
    child.wait_with_output().into_diagnostic()
}
