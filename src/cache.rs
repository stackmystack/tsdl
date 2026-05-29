//! Build cache: TOML-backed key-value store indexed by `{language}/{grammar}`.
//! Entries store grammar hashes and build specs to decide when rebuilds are
//! needed.

use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
    io::{self, Write as _},
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tokio::io::AsyncWrite;
use tracing::debug;

use crate::{args, build, build::BuildDir, git, parser, Error, Result, ResultExt};

// ============================================================
// Enums
// ============================================================

/// A cache lookup result for a requested parser build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Entry exists and is fully valid.
    Hit,
    /// Entry is missing or something changed.
    Miss(Miss),
}

/// One reason a cached parser build cannot be reused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissReason {
    /// No cache entry at all.
    MissingEntry,
    /// Cache was explicitly bypassed (--force).
    CacheIgnored,
    /// Hash of grammar.js changed.
    HashChanged {
        cached: GrammarHash,
        current: GrammarHash,
    },
    /// Parser repo URL changed.
    RepoChanged { cached: String, current: String },
    /// Git ref changed (different tag or branch).
    RefChanged {
        cached: parser::Ref,
        current: parser::Ref,
    },
    /// Git ref resolved to a different commit.
    RevisionChanged { cached: Revision, current: Revision },
    /// Tree-sitter CLI version changed.
    TreeSitterChanged {
        cached: args::TreeSitter,
        current: args::TreeSitter,
    },
    /// Build script changed.
    BuildScriptChanged,
    /// Output filename prefix changed.
    PrefixChanged { cached: String, current: String },
    /// Requested output target not covered by cached entry.
    OutputsMissing {
        available: args::Target,
        requested: args::Target,
    },
    /// Expected output file is missing from disk.
    ArtifactMissing { path: PathBuf },
    /// Expected output path exists but is not a regular file.
    ArtifactNotFile { path: PathBuf },
    /// Expected output path exists but cannot be read.
    ArtifactInaccessible { path: PathBuf, error: String },
}

/// Parser revision identity: stable (tag/commit) or moving (branch with
/// pinned commit SHA).
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Revision {
    /// Stable ref — no per-build commit tracking needed.
    Stable,
    /// Moving ref — the specific commit SHA checked out this build.
    Moving { commit: git::Sha },
}

// ============================================================
// Structs
// ============================================================

/// The logical build cache contents.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Db {
    /// Per-key cache entries (keyed by `"{language}/{grammar}"`).
    #[serde(default)]
    pub parsers: BTreeMap<Key, Entry>,
}

/// Cache entry for a single parser.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Hash of the grammar.js file(s).
    pub hash: GrammarHash,
    /// Resolved parser revision used by the cache. Moving refs include the
    /// checked-out commit.
    pub revision: Revision,
    /// Build specification used to produce this entry.
    pub spec: Arc<build::Spec>,
    /// Parser outputs known to be available for this entry.
    pub outputs: args::Target,
}

/// SHA-1 hash of a grammar.js file.
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GrammarHash(Arc<str>);

struct HashWriter<'a>(&'a mut Sha1);

/// Cache lookup key: `"{language}/{grammar}"`.
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Key(Arc<str>);

/// Details explaining why a cache entry cannot satisfy a requested parser build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Miss {
    /// Individual reasons for the cache miss.
    pub reasons: Vec<MissReason>,
}

/// File-backed storage for a [`Db`].
#[derive(Debug, Clone)]
pub struct Store {
    /// Path to the cache file on disk.
    file: PathBuf,
}

/// Represents a "Delta" to be applied to the cache after a successful build
#[derive(Debug, Clone)]
pub struct Update {
    /// The cache entry to store.
    pub entry: Entry,
    /// The key to store it under.
    pub name: Key,
}

// ============================================================
// Free functions
// ============================================================

/// Hash the contents of a file using SHA-1 and return the hex string.
pub async fn hash_file(path: &Path) -> Result<GrammarHash> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("Opening file for hashing: {}", path.display()))?;

    let mut hasher = Sha1::new();

    tokio::io::copy(&mut file, &mut HashWriter(&mut hasher))
        .await
        .with_context(|| format!("Reading file for hashing: {}", path.display()))?;

    let result = hasher.finalize();
    let mut hex = String::with_capacity(result.len() * 2);
    for byte in result {
        write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(GrammarHash::from(hex))
}

/// Sync the directory metadata to disk (best-effort for atomic write safety).
fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

/// Verify that every path in `artifacts` exists as a regular file.
///
/// Returns [`Decision::Hit`] when all artifacts are present, or a
/// [`Decision::Miss`] listing every missing or non-file path.
pub async fn verify_artifacts(artifacts: Vec<PathBuf>) -> Decision {
    let mut reasons = Vec::new();

    for path in artifacts {
        match tokio::fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => reasons.push(MissReason::ArtifactNotFile { path }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                reasons.push(MissReason::ArtifactMissing { path });
            }
            Err(err) => reasons.push(MissReason::ArtifactInaccessible {
                path,
                error: err.to_string(),
            }),
        }
    }

    Decision::from_reasons(reasons)
}

/// Atomically write cache contents to a file using a temporary file + rename.
fn write_cache_file_atomically(file: &Path, contents: &str) -> Result<()> {
    let parent = file.parent().ok_or_else(|| Error::Message {
        message: format!(
            "Cache file path has no parent directory: {}",
            file.display()
        ),
    })?;

    std::fs::create_dir_all(parent)
        .with_context(|| format!("Creating cache directory {}", parent.display()))?;

    let mut temp = tempfile::Builder::new()
        .prefix(".cache.toml.")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("Creating temporary cache file in {}", parent.display()))?;

    temp.write_all(contents.as_bytes())
        .with_context(|| format!("Writing temporary cache file {}", temp.path().display()))?;
    temp.as_file_mut()
        .sync_all()
        .with_context(|| format!("Syncing temporary cache file {}", temp.path().display()))?;

    temp.persist(file)
        .map_err(|err| err.error)
        .with_context(|| format!("Installing cache file to {}", file.display()))?;

    if let Err(err) = sync_directory(parent) {
        debug!(
            "Could not sync cache directory {} after saving {}: {err}",
            parent.display(),
            file.display()
        );
    }

    Ok(())
}

// ============================================================
// Impls
// ============================================================

impl AsyncWrite for HashWriter<'_> {
    /// Write bytes into the SHA-1 hasher.
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.0.update(buf);
        Poll::Ready(Ok(buf.len()))
    }

    /// Flush is a no-op for the hasher.
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    /// Shutdown is a no-op for the hasher.
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Db {
    /// Clear all entries.
    pub fn clear(&mut self) {
        self.parsers.clear();
    }

    /// Get cache entry for a parser.
    #[must_use]
    pub fn get(&self, name: &Key) -> Option<&Entry> {
        self.parsers.get(name)
    }

    /// True when the cache contains at least one entry for `language`
    /// that was built from the same repository and git ref.
    ///
    /// The tree-sitter CLI version, build script, prefix, and target are
    /// intentionally excluded — they affect artefact validity (checked by
    /// `needs_rebuild`) but not whether a prior checkout of the parser
    /// source is still usable.
    #[must_use]
    pub fn has_compatible_entry_for_language(
        &self,
        language: &parser::LanguageName,
        spec: &build::Spec,
    ) -> bool {
        let prefix = Key::language_prefix(language);
        self.parsers.iter().any(|(key, entry)| {
            key.as_str().starts_with(&prefix)
                && entry.spec.repo == spec.repo
                && entry.spec.git_ref == spec.git_ref
        })
    }

    /// Check if a parser needs rebuilding by comparing grammar hash and build definition.
    #[must_use]
    pub fn needs_rebuild(
        &self,
        name: &Key,
        hash: &GrammarHash,
        revision: &Revision,
        spec: &build::Spec,
    ) -> bool {
        self.rebuild_decision(name, hash, revision, spec)
            .needs_rebuild()
    }

    /// Explain whether a parser cache entry can satisfy the requested build.
    pub fn rebuild_decision(
        &self,
        name: &Key,
        hash: &GrammarHash,
        revision: &Revision,
        spec: &build::Spec,
    ) -> Decision {
        let decision = match self.get(name) {
            None => Decision::miss(MissReason::MissingEntry),
            Some(entry) => entry.rebuild_decision(hash, revision, spec),
        };

        debug!("Cache decision for {name}: {decision}");
        decision
    }

    /// Insert or update a parser cache entry.
    pub fn set(&mut self, name: Key, mut entry: Entry) {
        if let Some(existing) = self.parsers.get(&name) {
            if existing.same_subject(&entry) {
                entry.outputs = existing.outputs.union(entry.outputs);
            }
        }

        self.parsers.insert(name, entry);
    }
}

impl Decision {
    /// Create a Decision from a list of miss reasons (empty list → Hit).
    #[must_use]
    pub fn from_reasons(reasons: Vec<MissReason>) -> Self {
        if reasons.is_empty() {
            Self::Hit
        } else {
            Self::Miss(Miss { reasons })
        }
    }

    /// Check whether this decision indicates a cache hit.
    #[must_use]
    pub fn is_hit(&self) -> bool {
        matches!(self, Self::Hit)
    }

    /// Create a Miss decision for a single reason.
    #[must_use]
    pub fn miss(reason: MissReason) -> Self {
        Self::Miss(Miss {
            reasons: vec![reason],
        })
    }

    /// Check whether the decision requires a rebuild.
    #[must_use]
    pub fn needs_rebuild(&self) -> bool {
        !self.is_hit()
    }

    /// Return a short human-readable summary of the decision.
    #[must_use]
    pub fn short_message(&self) -> String {
        match self {
            Self::Hit => "cache hit".to_string(),
            Self::Miss(miss) => miss.short_message(),
        }
    }
}

impl Entry {
    /// Compare the current build parameters against this entry and decide
    /// whether the cached output is still valid.
    #[must_use]
    pub fn rebuild_decision(
        &self,
        hash: &GrammarHash,
        revision: &Revision,
        spec: &build::Spec,
    ) -> Decision {
        let mut reasons = Vec::new();

        if &self.hash != hash {
            reasons.push(MissReason::HashChanged {
                cached: self.hash.clone(),
                current: hash.clone(),
            });
        }

        if self.spec.repo != spec.repo {
            reasons.push(MissReason::RepoChanged {
                cached: self.spec.repo.to_string(),
                current: spec.repo.to_string(),
            });
        }

        let git_ref_changed = self.spec.git_ref != spec.git_ref;
        if git_ref_changed {
            reasons.push(MissReason::RefChanged {
                cached: self.spec.git_ref.clone(),
                current: spec.git_ref.clone(),
            });
        }

        if !git_ref_changed && self.revision != *revision {
            reasons.push(MissReason::RevisionChanged {
                cached: self.revision.clone(),
                current: revision.clone(),
            });
        }

        if self.spec.tree_sitter != spec.tree_sitter {
            reasons.push(MissReason::TreeSitterChanged {
                cached: self.spec.tree_sitter.clone(),
                current: spec.tree_sitter.clone(),
            });
        }

        if self.spec.build_script != spec.build_script {
            reasons.push(MissReason::BuildScriptChanged);
        }

        if self.spec.prefix != spec.prefix {
            reasons.push(MissReason::PrefixChanged {
                cached: self.spec.prefix.clone(),
                current: spec.prefix.clone(),
            });
        }

        if !self.outputs.covers(spec.target) {
            reasons.push(MissReason::OutputsMissing {
                available: self.outputs,
                requested: spec.target,
            });
        }

        Decision::from_reasons(reasons)
    }

    /// Check whether two entries describe the same build subject (ignoring
    /// output coverage).
    #[must_use]
    pub fn same_subject(&self, other: &Self) -> bool {
        self.hash == other.hash
            && self.revision == other.revision
            && self.spec.build_script == other.spec.build_script
            && self.spec.git_ref == other.spec.git_ref
            && self.spec.prefix == other.spec.prefix
            && self.spec.repo == other.spec.repo
            && self.spec.tree_sitter == other.spec.tree_sitter
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Decision::Hit => write!(f, "cache hit"),
            Decision::Miss(miss) => write!(f, "cache miss: {miss}"),
        }
    }
}

impl fmt::Display for GrammarHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for Miss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, reason) in self.reasons.iter().enumerate() {
            if index > 0 {
                write!(f, "; ")?;
            }
            write!(f, "{reason}")?;
        }
        Ok(())
    }
}

impl fmt::Display for MissReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEntry => write!(f, "missing cache entry"),
            Self::CacheIgnored => write!(f, "cache ignored"),
            Self::HashChanged { cached, current } => {
                write!(f, "grammar hash changed cached={cached} current={current}")
            }
            Self::RepoChanged { cached, current } => {
                write!(f, "repo changed cached={cached} current={current}")
            }
            Self::RefChanged { cached, current } => write!(
                f,
                "git ref changed cached={} current={}",
                cached.requested().as_str(),
                current.requested().as_str()
            ),
            Self::RevisionChanged { cached, current } => {
                write!(f, "revision changed cached={cached} current={current}")
            }
            Self::TreeSitterChanged { cached, current } => write!(
                f,
                "tree-sitter changed cached=version:{} repo:{} platform:{} current=version:{} repo:{} platform:{}",
                cached.version,
                cached.repo,
                cached.platform,
                current.version,
                current.repo,
                current.platform
            ),
            Self::BuildScriptChanged => write!(f, "build script changed"),
            Self::PrefixChanged { cached, current } => {
                write!(f, "prefix changed cached={cached:?} current={current:?}")
            }
            Self::OutputsMissing {
                available,
                requested,
            } => {
                write!(
                    f,
                    "requested output not cached available={available:?} requested={requested:?}"
                )
            }
            Self::ArtifactMissing { path } => {
                write!(f, "artifact missing path={}", path.display())
            }
            Self::ArtifactNotFile { path } => {
                write!(f, "artifact is not a regular file path={}", path.display())
            }
            Self::ArtifactInaccessible { path, error } => {
                write!(f, "artifact inaccessible path={} error={error}", path.display())
            }
        }
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stable => write!(f, "stable"),
            Self::Moving { commit } => write!(f, "moving commit={}", commit.as_str()),
        }
    }
}

impl From<&str> for GrammarHash {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl From<&str> for Key {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl From<String> for GrammarHash {
    fn from(value: String) -> Self {
        Self(value.into())
    }
}

impl From<String> for Key {
    fn from(value: String) -> Self {
        Self(value.into())
    }
}

impl GrammarHash {
    /// Return the hash as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Key {
    /// Return the key as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Build the prefix for all keys belonging to a given language.
    #[must_use]
    pub fn language_prefix(language: &parser::LanguageName) -> String {
        format!("{language}/")
    }

    /// Create a cache key from a language name and grammar name.
    #[must_use]
    pub fn new(language: &parser::LanguageName, grammar: &parser::GrammarName) -> Self {
        Self(Arc::from(format!("{language}/{grammar}")))
    }
}

impl Miss {
    /// Return a short human-readable summary of the miss reasons.
    #[must_use]
    pub fn short_message(&self) -> String {
        match self.reasons.as_slice() {
            [] => "cache changed".to_string(),
            [reason] => reason.short_message().to_string(),
            reasons => {
                let labels = reasons
                    .iter()
                    .map(MissReason::short_label)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("cache changed: {labels}")
            }
        }
    }
}

impl MissReason {
    /// Return a compact label for this miss reason (suitable for lists).
    #[must_use]
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::MissingEntry => "missing",
            Self::CacheIgnored => "ignored",
            Self::HashChanged { .. } => "hash",
            Self::RepoChanged { .. } => "repo",
            Self::RefChanged { .. } => "ref",
            Self::RevisionChanged { .. } => "revision",
            Self::TreeSitterChanged { .. } => "tree-sitter",
            Self::BuildScriptChanged => "script",
            Self::PrefixChanged { .. } => "prefix",
            Self::OutputsMissing { .. } => "outputs",
            Self::ArtifactMissing { .. }
            | Self::ArtifactNotFile { .. }
            | Self::ArtifactInaccessible { .. } => "artifact",
        }
    }

    /// Return a one-line message describing this miss reason.
    #[must_use]
    pub fn short_message(&self) -> &'static str {
        match self {
            Self::MissingEntry => "not cached",
            Self::CacheIgnored => "cache ignored",
            Self::HashChanged { .. } => "grammar changed",
            Self::RepoChanged { .. } => "repo changed",
            Self::RefChanged { .. } => "git ref changed",
            Self::RevisionChanged { .. } => "git ref resolved commit changed",
            Self::TreeSitterChanged { .. } => "tree-sitter changed",
            Self::BuildScriptChanged => "build script changed",
            Self::PrefixChanged { .. } => "prefix changed",
            Self::OutputsMissing { .. } => "requested output not cached",
            Self::ArtifactMissing { .. } => "artifact missing",
            Self::ArtifactNotFile { .. } => "artifact invalid",
            Self::ArtifactInaccessible { .. } => "artifact inaccessible",
        }
    }
}

impl Revision {
    /// Create a stable (non-moving) revision.
    #[must_use]
    pub const fn stable() -> Self {
        Self::Stable
    }

    /// Create a moving revision pinned to a specific commit.
    #[must_use]
    pub const fn moving(commit: git::Sha) -> Self {
        Self::Moving { commit }
    }
}

impl Store {
    /// Create a store backed by the build directory's cache file.
    #[must_use]
    pub fn new(build_dir: &BuildDir) -> Self {
        Self {
            file: build_dir.cache_file(),
        }
    }

    /// Return the path to the cache file on disk.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.file
    }

    /// Delete the cache file from disk.
    pub async fn delete(&self) -> Result<()> {
        match tokio::fs::metadata(&self.file).await {
            Ok(_) => {
                tokio::fs::remove_file(&self.file)
                    .await
                    .with_context(|| format!("Deleting cache file at {}", self.file.display()))?;
                debug!("Cache file deleted");
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("Reading cache file metadata at {}", self.file.display())
                });
            }
        }

        Ok(())
    }

    /// Load the cache from disk, or return an empty cache.
    pub fn load(&self) -> Result<Db> {
        if !self.file.exists() {
            debug!(
                "Cache file not found at {}, returning empty cache",
                self.file.display()
            );
            return Ok(Db::default());
        }

        let contents = std::fs::read_to_string(&self.file)
            .with_context(|| format!("Reading cache file at {}", self.file.display()))?;

        toml::from_str(&contents)
            .with_context(|| format!("Parsing cache file at {}", self.file.display()))
    }

    /// Atomically save the cache to disk.
    pub async fn save(&self, db: &Db) -> Result<()> {
        let contents = toml::to_string_pretty(db).context("Serializing cache to TOML")?;
        let file = self.file.clone();
        let result =
            tokio::task::spawn_blocking(move || write_cache_file_atomically(&file, &contents))
                .await
                .context("Joining cache save task")?;
        result?;

        debug!("Cache saved to {}", self.file.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{consts, git, parser};

    const SHA1: &str = "636801770eea172d140e64b691815ff11f6b556f";
    const SHA2: &str = "736801770eea172d140e64b691815ff11f6b556f";

    fn test_spec() -> build::Spec {
        build::Spec {
            build_script: None,
            git_ref: parser::Ref::parse("v1.0.0").unwrap(),
            repo: "https://github.com/example/parser".parse().unwrap(),
            tree_sitter: args::TreeSitter::default(),
            prefix: String::new(),
            target: args::Target::Native,
        }
    }

    fn moving_spec() -> build::Spec {
        build::Spec {
            git_ref: parser::Ref::parse("master").unwrap(),
            ..test_spec()
        }
    }

    fn stable_revision() -> Revision {
        Revision::stable()
    }

    fn moving_revision(spec: &build::Spec, sha: &str) -> Revision {
        assert!(spec.git_ref.is_moving());
        Revision::moving(git::Sha::new(sha).unwrap())
    }

    fn key() -> Key {
        Key::from("test-parser")
    }

    fn grammar_hash(value: &str) -> GrammarHash {
        GrammarHash::from(value)
    }

    fn entry(hash: &str, spec: &build::Spec, revision: Revision) -> Entry {
        Entry {
            hash: grammar_hash(hash),
            revision,
            spec: Arc::new(spec.clone()),
            outputs: spec.target,
        }
    }

    fn cache_with_entry(hash: &str, spec: &build::Spec) -> Db {
        cache_with_entry_and_revision(hash, spec, stable_revision())
    }

    fn cache_with_entry_and_revision(hash: &str, spec: &build::Spec, revision: Revision) -> Db {
        let mut cache = Db::default();
        cache.set(key(), entry(hash, spec, revision));
        cache
    }

    fn assert_miss(decision: Decision, expected: &[MissReason]) {
        match decision {
            Decision::Hit => panic!("expected cache miss"),
            Decision::Miss(miss) => assert_eq!(miss.reasons, expected),
        }
    }

    #[test]
    fn test_rebuild_decision_no_entry() {
        let cache = Db::default();
        let spec = test_spec();

        assert_miss(
            cache.rebuild_decision(&key(), &grammar_hash("abc123"), &stable_revision(), &spec),
            &[MissReason::MissingEntry],
        );
    }

    #[test]
    fn test_rebuild_decision_hash_mismatch() {
        let spec = test_spec();
        let cache = cache_with_entry("abc123", &spec);

        assert_miss(
            cache.rebuild_decision(&key(), &grammar_hash("def456"), &stable_revision(), &spec),
            &[MissReason::HashChanged {
                cached: grammar_hash("abc123"),
                current: grammar_hash("def456"),
            }],
        );
    }

    #[test]
    fn test_rebuild_decision_git_ref_mismatch() {
        let cached = test_spec();
        let mut requested = cached.clone();
        requested.git_ref = parser::Ref::parse("v2.0.0").unwrap();
        let cache = cache_with_entry("abc123", &cached);

        assert_miss(
            cache.rebuild_decision(
                &key(),
                &grammar_hash("abc123"),
                &stable_revision(),
                &requested,
            ),
            &[MissReason::RefChanged {
                cached: parser::Ref::parse("v1.0.0").unwrap(),
                current: parser::Ref::parse("v2.0.0").unwrap(),
            }],
        );
    }

    #[test]
    fn test_rebuild_decision_outputs_must_cover_requested_target() {
        let cached = test_spec();
        let mut requested = cached.clone();
        requested.target = args::Target::Wasm;
        let cache = cache_with_entry("abc123", &cached);

        assert_miss(
            cache.rebuild_decision(
                &key(),
                &grammar_hash("abc123"),
                &stable_revision(),
                &requested,
            ),
            &[MissReason::OutputsMissing {
                available: args::Target::Native,
                requested: args::Target::Wasm,
            }],
        );
    }

    #[test]
    fn test_rebuild_decision_all_outputs_cover_narrower_targets() {
        let mut cached = test_spec();
        cached.target = args::Target::All;
        let cache = cache_with_entry("abc123", &cached);

        for target in [args::Target::Native, args::Target::Wasm] {
            let mut requested = cached.clone();
            requested.target = target;
            assert_eq!(
                cache.rebuild_decision(
                    &key(),
                    &grammar_hash("abc123"),
                    &stable_revision(),
                    &requested,
                ),
                Decision::Hit
            );
        }
    }

    #[test]
    fn test_rebuild_decision_cache_hit_exact() {
        let spec = test_spec();
        let cache = cache_with_entry("abc123", &spec);

        assert_eq!(
            cache.rebuild_decision(&key(), &grammar_hash("abc123"), &stable_revision(), &spec),
            Decision::Hit
        );
        assert!(!cache.needs_rebuild(&key(), &grammar_hash("abc123"), &stable_revision(), &spec));
    }

    #[test]
    fn test_rebuild_decision_moving_revision_commit_changed() {
        let spec = moving_spec();
        let cached_revision = moving_revision(&spec, SHA1);
        let current_revision = moving_revision(&spec, SHA2);
        let cache = cache_with_entry_and_revision("abc123", &spec, cached_revision.clone());

        assert_miss(
            cache.rebuild_decision(&key(), &grammar_hash("abc123"), &current_revision, &spec),
            &[MissReason::RevisionChanged {
                cached: cached_revision,
                current: current_revision,
            }],
        );
    }

    #[test]
    fn test_rebuild_decision_moving_revision_commit_unchanged_hits() {
        let spec = moving_spec();
        let revision = moving_revision(&spec, SHA1);
        let cache = cache_with_entry_and_revision("abc123", &spec, revision.clone());

        assert_eq!(
            cache.rebuild_decision(&key(), &grammar_hash("abc123"), &revision, &spec),
            Decision::Hit
        );
    }

    #[test]
    fn test_rebuild_decision_collects_multiple_reasons() {
        let cached = test_spec();
        let mut requested = cached.clone();
        requested.build_script = Some("make".to_string());
        requested.prefix = "custom-".to_string();
        requested.target = args::Target::All;
        let cache = cache_with_entry("abc123", &cached);

        assert_miss(
            cache.rebuild_decision(
                &key(),
                &grammar_hash("def456"),
                &stable_revision(),
                &requested,
            ),
            &[
                MissReason::HashChanged {
                    cached: grammar_hash("abc123"),
                    current: grammar_hash("def456"),
                },
                MissReason::BuildScriptChanged,
                MissReason::PrefixChanged {
                    cached: String::new(),
                    current: "custom-".to_string(),
                },
                MissReason::OutputsMissing {
                    available: args::Target::Native,
                    requested: args::Target::All,
                },
            ],
        );
    }

    #[test]
    fn set_merges_outputs_for_same_entry_subject() {
        let native = test_spec();
        let mut wasm = native.clone();
        wasm.target = args::Target::Wasm;
        let mut cache = Db::default();

        cache.set(key(), entry("abc123", &native, stable_revision()));
        cache.set(key(), entry("abc123", &wasm, stable_revision()));

        assert_eq!(cache.get(&key()).unwrap().outputs, args::Target::All);
    }

    #[test]
    fn set_replaces_when_recipe_changes() {
        let native = test_spec();
        let mut wasm = native.clone();
        wasm.target = args::Target::Wasm;
        wasm.prefix = "other-".to_string();
        let mut cache = Db::default();

        cache.set(key(), entry("abc123", &native, stable_revision()));
        cache.set(key(), entry("abc123", &wasm, stable_revision()));

        let stored = cache.get(&key()).unwrap();
        assert_eq!(stored.outputs, args::Target::Wasm);
        assert_eq!(stored.spec.prefix, "other-");
    }

    #[test]
    fn compatible_entries_use_output_coverage() {
        let mut cached = test_spec();
        cached.target = args::Target::All;
        let mut requested = cached.clone();
        requested.target = args::Target::Native;
        let mut cache = Db::default();

        cache.set(
            Key::from("rust/rust"),
            entry("abc123", &cached, stable_revision()),
        );

        assert!(cache
            .has_compatible_entry_for_language(&parser::LanguageName::from("rust"), &requested));
    }

    #[test]
    fn store_loads_cache_without_runtime_file_path() {
        let temp = tempfile::TempDir::new().unwrap();
        let build_dir = BuildDir::new(temp.path()).unwrap();
        let store = Store::new(&build_dir);
        let spec = test_spec();
        let cache = cache_with_entry("abc123", &spec);
        let contents = toml::to_string_pretty(&cache).unwrap();

        std::fs::write(store.path(), contents).unwrap();

        let loaded = store.load().unwrap();
        assert!(loaded.get(&key()).is_some());
    }

    #[tokio::test]
    async fn store_ignores_legacy_file_field_and_saves_to_current_path() {
        let current = tempfile::TempDir::new().unwrap();
        let stale = tempfile::TempDir::new().unwrap();
        let build_dir = BuildDir::new(current.path()).unwrap();
        let store = Store::new(&build_dir);
        let stale_file = stale.path().join(consts::CACHE_FILE);
        let spec = test_spec();
        let cache = cache_with_entry("abc123", &spec);
        let contents = toml::to_string_pretty(&cache).unwrap();
        let legacy_contents = format!(
            "file = {:?}\n{contents}",
            stale_file.to_string_lossy().as_ref()
        );

        std::fs::write(store.path(), legacy_contents).unwrap();

        let mut loaded = store.load().unwrap();
        assert!(loaded.get(&key()).is_some());
        loaded.clear();

        store.save(&loaded).await.unwrap();

        assert!(
            !stale_file.exists(),
            "cache save should not use a legacy serialized file path"
        );
        let saved = std::fs::read_to_string(store.path()).unwrap();
        assert!(
            !saved.lines().any(|line| line.starts_with("file")),
            "cache TOML should not serialize runtime storage path: {saved}"
        );
        assert!(saved.contains("parsers"));
    }

    #[tokio::test]
    async fn verify_artifacts_hits_when_all_paths_are_files() {
        let temp = tempfile::TempDir::new().unwrap();
        let artifact = temp.path().join("parser.so");
        tokio::fs::write(&artifact, b"parser").await.unwrap();

        assert_eq!(verify_artifacts(vec![artifact]).await, Decision::Hit);
    }

    #[tokio::test]
    async fn verify_artifacts_reports_missing_and_non_file_paths() {
        let temp = tempfile::TempDir::new().unwrap();
        let missing = temp.path().join("missing.so");
        let directory = temp.path().join("parser.so");
        tokio::fs::create_dir(&directory).await.unwrap();

        assert_eq!(
            verify_artifacts(vec![missing.clone(), directory.clone()]).await,
            Decision::Miss(Miss {
                reasons: vec![
                    MissReason::ArtifactMissing { path: missing },
                    MissReason::ArtifactNotFile { path: directory },
                ],
            })
        );
    }
}
