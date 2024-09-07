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
    if cwd.exists() {
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
    } else {
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
    }
    Ok(())
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
