//! Git operations: clone, fetch, checkout, ls-files, tag resolution.

use std::{
    ffi::OsStr,
    fmt,
    path::{Component, Path, PathBuf},
    result::Result as StdResult,
    sync::Arc,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use tokio::{fs, process::Command};

use crate::{Error, Result, ResultExt, sh::Exec};

type RefResult<T> = StdResult<T, RefError>;

// ============================================================
// Enums
// ============================================================

/// Error type for git ref and SHA validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefError {
    /// Ref string is empty.
    EmptyRef,
    /// Ref contains an invalid character.
    InvalidRefCharacter { index: usize, character: char },
    /// Ref has invalid syntax.
    InvalidRefSyntax { reason: &'static str },
    /// SHA string is not 40 characters long.
    InvalidShaLength { actual: usize },
    /// SHA string contains non-hex characters.
    InvalidShaHex { index: usize, character: char },
}

/// A resolved git ref after tag lookup or branch checkout.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum ResolvedRef {
    /// A tag with its resolved commit SHA.
    Tag { label: String, sha: Sha },
    /// A bare git ref (no tag resolution).
    Ref(Ref),
}

// ============================================================
// Structs
// ============================================================

/// The commit SHA checked out for a given git ref.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Checkout {
    /// The checked-out commit hash.
    pub commit: Sha,
}

/// A validated git ref string (branch name, tag, or commit SHA).
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Ref(Arc<str>);

/// A validated 40-character hex SHA-1 commit hash.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Sha(Arc<str>);

// ============================================================
// Free functions
// ============================================================

/// Check out a git ref in the given directory (with force flag to handle
/// remote URL changes).
pub async fn checkout(repo: &str, cwd: &Path, git_ref: &Ref) -> Result<Checkout> {
    checkout_with_force(repo, cwd, git_ref, false).await
}

/// Like [`checkout`], but optionally force-clean the working directory even
/// when the remote URL matches.
pub async fn checkout_with_force(
    repo: &str,
    cwd: &Path,
    git_ref: &Ref,
    force: bool,
) -> Result<Checkout> {
    if force || !is_same_remote(cwd, repo).await {
        clean_anyway(cwd).await?;
    }
    if is_valid_git_dir(cwd).await {
        reset_head_hard(cwd, git_ref).await?;
    } else {
        init_fetch_and_checkout(cwd, repo, git_ref).await?;
    }
    let commit = get_head_sha(cwd)
        .await
        .with_context(|| format!("Resolving checked out commit for {}", cwd.display()))?;
    Ok(Checkout { commit })
}

// TODO: get rid of async fs completely.
/// Remove a directory or file at `cwd` if it exists.
async fn clean_anyway(cwd: &Path) -> Result<()> {
    if cwd.exists() {
        if cwd.is_dir() {
            fs::remove_dir_all(cwd).await
        } else {
            fs::remove_file(cwd).await
        }?;
    }
    Ok(())
}

/// Clone a repository and pull if it already exists.
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

/// Fetch from origin and reset to `FETCH_HEAD`.
async fn fetch_and_checkout(cwd: &Path, git_ref: &Ref) -> Result<()> {
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

/// Get the full SHA of HEAD in the given repo.
async fn get_head_sha(cwd: &Path) -> Result<Sha> {
    let value = get_head_sha1(cwd).await?;
    Sha::new(value.trim()).context("Parsing HEAD commit")
}

/// Get the raw SHA-1 string of HEAD.
async fn get_head_sha1(cwd: &Path) -> Result<String> {
    String::from_utf8(
        Command::new("git")
            .current_dir(cwd)
            .args(["rev-parse", "HEAD"])
            .exec()
            .await?
            .stdout,
    )
    .context("rev-parse HEAD is not a valid utf-8")
}

/// Get the remote URL for origin.
async fn get_remote_url(cwd: &Path) -> Result<String> {
    String::from_utf8(
        Command::new("git")
            .current_dir(cwd)
            .args(["remote", "get-url", "origin"])
            .exec()
            .await?
            .stdout,
    )
    .context("remote get-url origin did not return a valid utf-8")
}

/// Initialise a new repo, add a remote, and fetch+checkout.
async fn init_fetch_and_checkout(cwd: &Path, repo: &str, git_ref: &Ref) -> Result<()> {
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

/// Check whether the checkout directory contains a usable git repo with the
/// expected remote URL.
pub async fn is_checkout_usable(repo: &str, cwd: &Path) -> bool {
    is_valid_git_dir(cwd).await && is_same_remote(cwd, repo).await
}

/// Check whether a directory has the same origin remote URL as `remote`.
async fn is_same_remote(cwd: &Path, remote: &str) -> bool {
    remote == get_remote_url(cwd).await.unwrap_or_default().trim()
}

/// Check whether `cwd` is inside a valid git work tree with a parseable HEAD.
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

/// List all grammar.js files tracked by git (excluding common non-source
/// directories).
pub async fn list_grammar_files(cwd: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .exec()
        .await?;

    let stdout =
        String::from_utf8(output.stdout).context("git ls-files output is not valid utf-8")?;

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

/// Reset HEAD hard to the given ref, fetching first if needed.
async fn reset_head_hard(cwd: &Path, git_ref: &Ref) -> Result<()> {
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

/// Resolve a git ref to a tag name or fall back to commit SHA.
pub async fn tag_for_ref(cwd: &Path, git_ref: &Ref) -> Result<String> {
    // Try to find a tag for this ref
    let tag = Command::new("git")
        .current_dir(cwd)
        .args(["describe", "--abbrev=0", "--tags", git_ref.as_str()])
        .exec()
        .await;

    if let Ok(output) = tag {
        // Found a tag, use it
        String::from_utf8(output.stdout)
            .context("Failed to parse git tag output as UTF-8")
            .map(|s| s.trim().to_string())
    } else {
        // No tag found (e.g., ref is a branch), fall back to commit SHA1
        let sha1 = Command::new("git")
            .current_dir(cwd)
            .args(["rev-parse", git_ref.as_str()])
            .exec()
            .await?;
        String::from_utf8(sha1.stdout)
            .context("Failed to parse git rev-parse output as UTF-8")
            .map(|s| s.trim().to_string())
    }
}

/// Validate a git ref string against Git's refname rules.
fn validate_git_ref(value: &str) -> RefResult<()> {
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

/// Validate a 40-character hex SHA-1 string.
fn validate_git_sha(value: &str) -> RefResult<()> {
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

// ============================================================
// Impls
// ============================================================

impl AsRef<str> for Ref {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Sha {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for Ref {
    fn deserialize<D>(deserializer: D) -> StdResult<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for Sha {
    fn deserialize<D>(deserializer: D) -> StdResult<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.short())
    }
}

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRef => write!(f, "git ref cannot be empty"),
            Self::InvalidRefCharacter { index, character } => {
                write!(
                    f,
                    "git ref contains invalid character at byte {index}: {character:?}"
                )
            }
            Self::InvalidRefSyntax { reason } => write!(f, "git ref is invalid: {reason}"),
            Self::InvalidShaLength { actual } => {
                write!(f, "git SHA must be exactly 40 hex characters, got {actual}")
            }
            Self::InvalidShaHex { index, character } => {
                write!(
                    f,
                    "git SHA contains non-hex character at byte {index}: {character:?}"
                )
            }
        }
    }
}

impl fmt::Display for ResolvedRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tag { label, .. } => write!(f, "{label}"),
            Self::Ref(git_ref) => write!(f, "{git_ref}"),
        }
    }
}

impl fmt::Display for Sha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.short())
    }
}

impl From<RefError> for Error {
    fn from(error: RefError) -> Self {
        Error::Message {
            message: error.to_string(),
        }
    }
}

impl From<Sha> for Ref {
    fn from(sha: Sha) -> Self {
        Self(sha.0)
    }
}

impl std::str::FromStr for Ref {
    type Err = RefError;

    fn from_str(value: &str) -> StdResult<Self, Self::Err> {
        Self::new(value)
    }
}

impl std::str::FromStr for Sha {
    type Err = RefError;

    fn from_str(value: &str) -> StdResult<Self, Self::Err> {
        Self::new(value)
    }
}

impl Ref {
    /// Create a validated git ref.
    pub fn new(value: impl Into<Arc<str>>) -> RefResult<Self> {
        let value = value.into();
        validate_git_ref(&value)?;
        Ok(Self(value))
    }

    /// The default ref used for unpinned parser builds.
    #[must_use]
    pub fn head() -> Self {
        Self(Arc::from("HEAD"))
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

    /// Check whether the ref is an exact 40-character SHA.
    #[must_use]
    pub fn is_exact_sha(&self) -> bool {
        Sha::is_full_sha(self.as_str())
    }
}

impl Serialize for Ref {
    fn serialize<S>(&self, serializer: S) -> StdResult<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl Serialize for Sha {
    fn serialize<S>(&self, serializer: S) -> StdResult<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl Sha {
    /// Create a validated full 40-character git SHA-1.
    pub fn new(value: impl Into<Arc<str>>) -> RefResult<Self> {
        let value = value.into();
        validate_git_sha(&value)?;
        Ok(Sha(value))
    }

    /// Return the SHA as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the first 7 hex characters of the SHA.
    #[must_use]
    pub fn short(&self) -> &str {
        &self.0[..7]
    }

    /// Check whether a string looks like a full 40-character hex SHA.
    #[must_use]
    pub fn is_full_sha(value: &str) -> bool {
        value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit())
    }
}

impl std::error::Error for RefError {}

impl TryFrom<&str> for Ref {
    type Error = RefError;

    fn try_from(value: &str) -> StdResult<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for Sha {
    type Error = RefError;

    fn try_from(value: &str) -> StdResult<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for Ref {
    type Error = RefError;

    fn try_from(value: String) -> StdResult<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for Sha {
    type Error = RefError;

    fn try_from(value: String) -> StdResult<Self, Self::Error> {
        Self::new(value)
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
                character: ' '
            })
        );
    }

    #[test]
    fn git_ref_rejects_invalid_git_ref_syntax() {
        assert_eq!(
            Ref::new("feature..branch"),
            Err(RefError::InvalidRefSyntax {
                reason: "refs cannot contain .."
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
    fn git_ref_preserves_branches_and_full_shas() {
        assert_eq!(Ref::new("master").unwrap().as_str(), "master");
        assert_eq!(Ref::new(FULL_SHA).unwrap().as_str(), FULL_SHA);
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
                character: 'x'
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
