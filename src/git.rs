use std::{
    ffi::OsStr,
    fmt,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use tokio::{fs, process::Command};

use crate::{error::TsdlError, sh::Exec, TsdlResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefError {
    EmptyRef,
    InvalidRefCharacter { index: usize, character: char },
    InvalidRefSyntax { reason: &'static str },
    InvalidShaLength { actual: usize },
    InvalidShaHex { index: usize, character: char },
}

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRef => write!(f, "git ref cannot be empty"),
            Self::InvalidRefCharacter { index, character } => write!(
                f,
                "git ref contains invalid character at byte {index}: {character:?}"
            ),
            Self::InvalidRefSyntax { reason } => write!(f, "git ref is invalid: {reason}"),
            Self::InvalidShaLength { actual } => {
                write!(f, "git SHA must be exactly 40 hex characters, got {actual}")
            }
            Self::InvalidShaHex { index, character } => write!(
                f,
                "git SHA contains non-hex character at byte {index}: {character:?}"
            ),
        }
    }
}

impl std::error::Error for RefError {}

impl From<RefError> for TsdlError {
    fn from(error: RefError) -> Self {
        TsdlError::message(error.to_string())
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Ref(Arc<str>);

impl Ref {
    /// Create a validated git ref.
    pub fn new(value: impl Into<Arc<str>>) -> Result<Self, RefError> {
        let value = value.into();
        validate_git_ref(&value)?;
        Ok(Self(value))
    }

    /// The default ref used for unpinned parser builds.
    #[must_use]
    pub fn head() -> Self {
        Self(Arc::from("HEAD"))
    }

    /// Create a validated git ref from user input, normalizing dotted numeric
    /// versions such as `0.21.0` to the conventional git tag `v0.21.0`.
    pub fn from_version_or_ref(value: &str) -> Result<Self, RefError> {
        if Sha::is_full_sha(value) || value.starts_with('v') {
            return Self::new(value);
        }

        if is_dotted_numeric_version(value) {
            Self::new(format!("v{value}"))
        } else {
            Self::new(value)
        }
    }

    /// Get the exact git ref string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Get a human-oriented representation, shortening full commit SHAs.
    #[must_use]
    pub fn short(&self) -> &str {
        if Sha::is_full_sha(self.as_str()) {
            &self.0[..7]
        } else {
            &self.0
        }
    }

    #[must_use]
    pub fn is_exact_sha(&self) -> bool {
        Sha::is_full_sha(self.as_str())
    }
}

impl AsRef<str> for Ref {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl TryFrom<&str> for Ref {
    type Error = RefError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for Ref {
    type Error = RefError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl std::str::FromStr for Ref {
    type Err = RefError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.short())
    }
}

impl Serialize for Ref {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Ref {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Sha(Arc<str>);

impl Sha {
    /// Create a validated full 40-character git SHA-1.
    pub fn new(value: impl Into<Arc<str>>) -> Result<Self, RefError> {
        let value = value.into();
        validate_git_sha(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn short(&self) -> &str {
        &self.0[..7]
    }

    #[must_use]
    pub fn is_full_sha(value: &str) -> bool {
        value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit())
    }
}

impl AsRef<str> for Sha {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl TryFrom<&str> for Sha {
    type Error = RefError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for Sha {
    type Error = RefError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl std::str::FromStr for Sha {
    type Err = RefError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for Sha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.short())
    }
}

impl Serialize for Sha {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Sha {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

impl From<Sha> for Ref {
    fn from(sha: Sha) -> Self {
        Self(sha.0)
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum ResolvedRef {
    Tag { label: String, sha: Sha },
    Ref(Ref),
}

impl fmt::Display for ResolvedRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tag { label, .. } => write!(f, "{label}"),
            Self::Ref(git_ref) => write!(f, "{git_ref}"),
        }
    }
}

fn validate_git_ref(value: &str) -> Result<(), RefError> {
    if value.is_empty() {
        return Err(RefError::EmptyRef);
    }

    if value == "@" {
        return Err(RefError::InvalidRefSyntax {
            reason: "single @ is not a ref",
        });
    }

    if value.starts_with('/') || value.ends_with('/') {
        return Err(RefError::InvalidRefSyntax {
            reason: "refs cannot start or end with /",
        });
    }

    if value.ends_with('.') {
        return Err(RefError::InvalidRefSyntax {
            reason: "refs cannot end with .",
        });
    }

    if value.contains("..") {
        return Err(RefError::InvalidRefSyntax {
            reason: "refs cannot contain ..",
        });
    }

    if value.contains("@{") {
        return Err(RefError::InvalidRefSyntax {
            reason: "refs cannot contain @{",
        });
    }

    if let Some((index, character)) = value.char_indices().find(|(_, c)| {
        c.is_ascii_control()
            || c.is_ascii_whitespace()
            || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
    }) {
        return Err(RefError::InvalidRefCharacter { index, character });
    }

    for component in value.split('/') {
        if component.is_empty() {
            return Err(RefError::InvalidRefSyntax {
                reason: "refs cannot contain empty path components",
            });
        }

        if component.starts_with('.') {
            return Err(RefError::InvalidRefSyntax {
                reason: "ref path components cannot start with .",
            });
        }

        if component.strip_suffix(".lock").is_some() {
            return Err(RefError::InvalidRefSyntax {
                reason: "ref path components cannot end with .lock",
            });
        }
    }

    Ok(())
}

fn validate_git_sha(value: &str) -> Result<(), RefError> {
    if value.len() != 40 {
        return Err(RefError::InvalidShaLength {
            actual: value.len(),
        });
    }

    if let Some((index, character)) = value.char_indices().find(|(_, c)| !c.is_ascii_hexdigit()) {
        return Err(RefError::InvalidShaHex { index, character });
    }

    Ok(())
}

fn is_dotted_numeric_version(value: &str) -> bool {
    !value.is_empty()
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.parse::<u32>().is_ok())
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

pub async fn clone_fast(repo: &str, git_ref: &Ref, cwd: &Path) -> TsdlResult<Sha> {
    clone_fast_with_force(repo, git_ref, cwd, false).await
}

pub async fn clone_fast_with_force(
    repo: &str,
    git_ref: &Ref,
    cwd: &Path,
    force: bool,
) -> TsdlResult<Sha> {
    if force || !is_same_remote(cwd, repo).await {
        clean_anyway(cwd).await?;
    }
    if is_valid_git_dir(cwd).await {
        reset_head_hard(cwd, git_ref).await?;
    } else {
        init_fetch_and_checkout(cwd, repo, git_ref).await?;
    }
    get_head_sha(cwd).await.map_err(|err| {
        TsdlError::context(
            format!("Resolving checked out commit for {}", cwd.display()),
            err,
        )
    })
}

async fn fetch_and_checkout(cwd: &Path, git_ref: &Ref) -> TsdlResult<()> {
    Command::new("git")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(cwd)
        .args(["fetch", "origin", "--depth", "1", git_ref.as_str()])
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

async fn get_head_sha(cwd: &Path) -> TsdlResult<Sha> {
    let value = get_head_sha1(cwd).await?;
    Sha::new(value.trim()).map_err(|e| TsdlError::context("Parsing HEAD commit", e))
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

async fn init_fetch_and_checkout(cwd: &Path, repo: &str, git_ref: &Ref) -> TsdlResult<()> {
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

async fn reset_head_hard(cwd: &Path, git_ref: &Ref) -> TsdlResult<()> {
    if git_ref.as_str() != get_head_sha1(cwd).await?.trim() {
        Command::new("git")
            .current_dir(cwd)
            .args(["reset", "--hard", "HEAD"])
            .exec()
            .await?;
        fetch_and_checkout(cwd, git_ref).await?;
    }
    Ok(())
}

pub async fn tag_for_ref(cwd: &Path, git_ref: &Ref) -> TsdlResult<String> {
    // Try to find a tag for this ref
    let tag = Command::new("git")
        .current_dir(cwd)
        .args(["describe", "--abbrev=0", "--tags", git_ref.as_str()])
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
            .args(["rev-parse", git_ref.as_str()])
            .exec()
            .await?;
        String::from_utf8(sha1.stdout)
            .map_err(|e| TsdlError::context("Failed to parse git rev-parse output as UTF-8", e))
            .map(|s| s.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_SHA: &str = "636801770eea172d140e64b691815ff11f6b556f";

    #[test]
    fn git_ref_rejects_empty_refs() {
        assert_eq!(Ref::new(""), Err(RefError::EmptyRef));
    }

    #[test]
    fn git_ref_rejects_whitespace() {
        assert_eq!(
            Ref::new("feature branch"),
            Err(RefError::InvalidRefCharacter {
                index: 7,
                character: ' ',
            })
        );
    }

    #[test]
    fn git_ref_rejects_invalid_git_ref_syntax() {
        assert_eq!(
            Ref::new("feature..branch"),
            Err(RefError::InvalidRefSyntax {
                reason: "refs cannot contain ..",
            })
        );
        assert_eq!(
            Ref::new("feature.lock"),
            Err(RefError::InvalidRefSyntax {
                reason: "ref path components cannot end with .lock",
            })
        );
        assert_eq!(
            Ref::new("refs/heads/main"),
            Ok(Ref::new("refs/heads/main").unwrap())
        );
    }

    #[test]
    fn git_ref_normalizes_dotted_numeric_versions() {
        assert_eq!(
            Ref::from_version_or_ref("0.21.0").unwrap().as_str(),
            "v0.21.0"
        );
        assert_eq!(
            Ref::from_version_or_ref("v0.21.0").unwrap().as_str(),
            "v0.21.0"
        );
    }

    #[test]
    fn git_ref_preserves_branches_and_full_shas() {
        assert_eq!(
            Ref::from_version_or_ref("master").unwrap().as_str(),
            "master"
        );
        assert_eq!(
            Ref::from_version_or_ref(FULL_SHA).unwrap().as_str(),
            FULL_SHA
        );
    }

    #[test]
    fn git_ref_display_is_short_but_as_str_is_exact() {
        let git_ref = Ref::new(FULL_SHA).unwrap();

        assert_eq!(git_ref.as_str(), FULL_SHA);
        assert_eq!(git_ref.short(), "6368017");
        assert_eq!(git_ref.to_string(), "6368017");
    }

    #[test]
    fn git_sha_requires_full_hex_sha() {
        assert_eq!(Sha::new(FULL_SHA).unwrap().as_str(), FULL_SHA);
        assert_eq!(Sha::new(FULL_SHA).unwrap().short(), "6368017");
        assert_eq!(
            Sha::new("6368017"),
            Err(RefError::InvalidShaLength { actual: 7 })
        );
        assert_eq!(
            Sha::new("636801770eea172d140e64b691815ff11f6b556x"),
            Err(RefError::InvalidShaHex {
                index: 39,
                character: 'x',
            })
        );
    }

    #[test]
    fn serde_rejects_invalid_git_refs() {
        let err = toml::from_str::<std::collections::BTreeMap<String, Ref>>(r#"git_ref = """#)
            .unwrap_err();

        assert!(err.to_string().contains("git ref cannot be empty"));
    }
}
