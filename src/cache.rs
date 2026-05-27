use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, ReadBuf};
use tracing::debug;

use crate::{args, build, consts, git, parser, Result, ResultExt};

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Key(Arc<str>);

impl Key {
    #[must_use]
    pub fn new(language: &parser::LanguageName, grammar: &parser::GrammarName) -> Self {
        Self(Arc::from(format!("{language}/{grammar}")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn language_prefix(language: &parser::LanguageName) -> String {
        format!("{language}/")
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for Key {
    fn from(value: String) -> Self {
        Self(value.into())
    }
}

impl From<&str> for Key {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GrammarHash(Arc<str>);

impl GrammarHash {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GrammarHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for GrammarHash {
    fn from(value: String) -> Self {
        Self(value.into())
    }
}

impl From<&str> for GrammarHash {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

/// The build cache stored in  `[build-dir]/[consts::CACHE_FILE]`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Db {
    pub parsers: BTreeMap<Key, Entry>,
    pub file: PathBuf,
}

/// Cache entry for a single parser
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Hash of the grammar.js file(s)
    pub hash: GrammarHash,
    /// Complete build definition that affects parser output
    pub spec: Arc<build::Spec>,
    /// Parser source identity used by the cache. Moving refs include the
    /// checked-out commit.
    pub source: Source,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Source {
    Stable,
    Moving { commit: git::Sha },
}

impl Source {
    #[must_use]
    pub const fn stable() -> Self {
        Self::Stable
    }

    #[must_use]
    pub const fn moving(commit: git::Sha) -> Self {
        Self::Moving { commit }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stable => write!(f, "stable"),
            Self::Moving { commit } => write!(f, "moving commit={}", commit.as_str()),
        }
    }
}

/// A cache lookup result for a requested parser build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Hit,
    Miss(Miss),
}

/// Details explaining why a cache entry cannot satisfy a requested parser build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Miss {
    pub reasons: Vec<MissReason>,
}

/// One reason a cached parser build cannot be reused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissReason {
    MissingEntry,
    CacheIgnored,
    HashChanged {
        cached: GrammarHash,
        current: GrammarHash,
    },
    RepoChanged {
        cached: String,
        current: String,
    },
    RefChanged {
        cached: parser::Ref,
        current: parser::Ref,
    },
    SourceChanged {
        cached: Source,
        current: Source,
    },
    TreeSitterChanged {
        cached: args::TreeSitter,
        current: args::TreeSitter,
    },
    BuildScriptChanged,
    PrefixChanged {
        cached: String,
        current: String,
    },
    TargetChanged {
        cached: args::Target,
        current: args::Target,
    },
    ArtifactMissing {
        path: PathBuf,
    },
    ArtifactNotFile {
        path: PathBuf,
    },
    ArtifactInaccessible {
        path: PathBuf,
        error: String,
    },
}

impl Decision {
    #[must_use]
    pub fn miss(reason: MissReason) -> Self {
        Self::Miss(Miss {
            reasons: vec![reason],
        })
    }

    #[must_use]
    pub fn from_reasons(reasons: Vec<MissReason>) -> Self {
        if reasons.is_empty() {
            Self::Hit
        } else {
            Self::Miss(Miss { reasons })
        }
    }

    #[must_use]
    pub fn is_hit(&self) -> bool {
        matches!(self, Self::Hit)
    }

    #[must_use]
    pub fn needs_rebuild(&self) -> bool {
        !self.is_hit()
    }

    #[must_use]
    pub fn short_message(&self) -> String {
        match self {
            Self::Hit => "cache hit".to_string(),
            Self::Miss(miss) => miss.short_message(),
        }
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

impl Miss {
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

impl MissReason {
    #[must_use]
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::MissingEntry => "missing",
            Self::CacheIgnored => "ignored",
            Self::HashChanged { .. } => "hash",
            Self::RepoChanged { .. } => "repo",
            Self::RefChanged { .. } => "ref",
            Self::SourceChanged { .. } => "source",
            Self::TreeSitterChanged { .. } => "tree-sitter",
            Self::BuildScriptChanged => "script",
            Self::PrefixChanged { .. } => "prefix",
            Self::TargetChanged { .. } => "target",
            Self::ArtifactMissing { .. }
            | Self::ArtifactNotFile { .. }
            | Self::ArtifactInaccessible { .. } => "artifact",
        }
    }

    #[must_use]
    pub fn short_message(&self) -> &'static str {
        match self {
            Self::MissingEntry => "not cached",
            Self::CacheIgnored => "cache ignored",
            Self::HashChanged { .. } => "grammar changed",
            Self::RepoChanged { .. } => "repo changed",
            Self::RefChanged { .. } => "git ref changed",
            Self::SourceChanged { .. } => "git ref resolved commit changed",
            Self::TreeSitterChanged { .. } => "tree-sitter changed",
            Self::BuildScriptChanged => "build script changed",
            Self::PrefixChanged { .. } => "prefix changed",
            Self::TargetChanged { .. } => "target changed",
            Self::ArtifactMissing { .. } => "artifact missing",
            Self::ArtifactNotFile { .. } => "artifact invalid",
            Self::ArtifactInaccessible { .. } => "artifact inaccessible",
        }
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
            Self::SourceChanged { cached, current } => {
                write!(f, "source changed cached={cached} current={current}")
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
            Self::TargetChanged { cached, current } => {
                write!(f, "target changed cached={cached:?} current={current:?}")
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

impl Entry {
    #[must_use]
    pub fn rebuild_decision(
        &self,
        hash: &GrammarHash,
        spec: &build::Spec,
        source: &Source,
    ) -> Decision {
        let mut reasons = Vec::new();
        let cached = self.spec.as_ref();

        if &self.hash != hash {
            reasons.push(MissReason::HashChanged {
                cached: self.hash.clone(),
                current: hash.clone(),
            });
        }

        if cached.repo != spec.repo {
            reasons.push(MissReason::RepoChanged {
                cached: cached.repo.to_string(),
                current: spec.repo.to_string(),
            });
        }

        let git_ref_changed = cached.git_ref != spec.git_ref;
        if git_ref_changed {
            reasons.push(MissReason::RefChanged {
                cached: cached.git_ref.clone(),
                current: spec.git_ref.clone(),
            });
        }

        if !git_ref_changed && self.source != *source {
            reasons.push(MissReason::SourceChanged {
                cached: self.source.clone(),
                current: source.clone(),
            });
        }

        if cached.tree_sitter != spec.tree_sitter {
            reasons.push(MissReason::TreeSitterChanged {
                cached: cached.tree_sitter.clone(),
                current: spec.tree_sitter.clone(),
            });
        }

        if cached.build_script != spec.build_script {
            reasons.push(MissReason::BuildScriptChanged);
        }

        if cached.prefix != spec.prefix {
            reasons.push(MissReason::PrefixChanged {
                cached: cached.prefix.clone(),
                current: spec.prefix.clone(),
            });
        }

        if cached.target != spec.target {
            reasons.push(MissReason::TargetChanged {
                cached: cached.target,
                current: spec.target,
            });
        }

        Decision::from_reasons(reasons)
    }
}

/// Represents a "Delta" to be applied to the cache after a successful build
#[derive(Debug, Clone)]
pub struct Update {
    pub entry: Entry,
    pub name: Key,
}

impl Db {
    /// Clear all entries
    pub fn clear(&mut self) {
        self.parsers.clear();
    }

    /// Delete the cache file from disk
    pub async fn delete(build_dir: &Path) -> Result<()> {
        let file = build_dir.join(consts::CACHE_FILE);
        if tokio::fs::metadata(&file).await.is_ok() {
            tokio::fs::remove_file(&file)
                .await
                .with_context(|| format!("Deleting cache file at {}", file.display()))?;
            debug!("Cache file deleted");
        }
        Ok(())
    }

    /// Get cache entry for a parser
    #[must_use]
    pub fn get(&self, name: &Key) -> Option<&Entry> {
        self.parsers.get(name)
    }

    /// Load the cache from disk, or return the empty cache.
    pub fn load(build_dir: &Path) -> Result<Self> {
        let file = build_dir.join(consts::CACHE_FILE);
        if !file.exists() {
            debug!(
                "Cache file not found at {}, returning empty cache",
                file.display()
            );
            return Ok(Db {
                parsers: BTreeMap::new(),
                file,
            });
        }

        let contents = std::fs::read_to_string(&file)
            .with_context(|| format!("Reading cache file at {}", file.display()))?;

        toml::from_str(&contents)
            .with_context(|| format!("Parsing cache file at {}", file.display()))
    }

    /// Explain whether a parser cache entry can satisfy the requested build.
    pub fn rebuild_decision(
        &self,
        name: &Key,
        hash: &GrammarHash,
        spec: &build::Spec,
        source: &Source,
    ) -> Decision {
        let decision = match self.get(name) {
            None => Decision::miss(MissReason::MissingEntry),
            Some(entry) => entry.rebuild_decision(hash, spec, source),
        };

        debug!("Cache decision for {name}: {decision}");
        decision
    }

    /// Check if a parser needs rebuilding by comparing grammar hash and build definition.
    #[must_use]
    pub fn needs_rebuild(
        &self,
        name: &Key,
        hash: &GrammarHash,
        spec: &build::Spec,
        source: &Source,
    ) -> bool {
        self.rebuild_decision(name, hash, spec, source)
            .needs_rebuild()
    }

    /// Save the cache to disk
    pub async fn save(&self) -> Result<()> {
        let contents = toml::to_string_pretty(self).context("Serializing cache to TOML")?;

        tokio::fs::write(&self.file, contents)
            .await
            .with_context(|| format!("Writing cache file to {}", self.file.display()))?;

        debug!("Cache saved to {}", self.file.display());
        Ok(())
    }

    /// Insert or update a parser cache entry
    pub fn set(&mut self, name: Key, entry: Entry) {
        self.parsers.insert(name, entry);
    }
}

/// Hash the contents of a file using SHA-1 and return the hex string.
pub async fn hash_file(path: &Path) -> Result<GrammarHash> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("Opening file for hashing: {}", path.display()))?;

    let mut hasher = Sha1::new();
    let mut buffer = vec![0u8; 8192];

    loop {
        let mut read_buf = ReadBuf::new(&mut buffer);
        file.read_buf(&mut read_buf)
            .await
            .with_context(|| format!("Reading file for hashing: {}", path.display()))?;

        if read_buf.filled().is_empty() {
            break;
        }

        hasher.update(read_buf.filled());
    }

    let result = hasher.finalize();
    let mut hex = String::with_capacity(result.len() * 2);
    for byte in result {
        write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(GrammarHash::from(hex))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{git, parser};

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

    fn stable_source(_spec: &build::Spec) -> Source {
        Source::stable()
    }

    fn moving_source(spec: &build::Spec, sha: &str) -> Source {
        assert!(spec.git_ref.is_moving());
        Source::moving(git::Sha::new(sha).unwrap())
    }

    fn key() -> Key {
        Key::from("test-parser")
    }

    fn grammar_hash(value: &str) -> GrammarHash {
        GrammarHash::from(value)
    }

    fn cache_with_entry(hash: &str, spec: &build::Spec) -> Db {
        cache_with_entry_and_source(hash, spec, stable_source(spec))
    }

    fn cache_with_entry_and_source(hash: &str, spec: &build::Spec, source: Source) -> Db {
        let mut cache = Db::default();
        cache.set(
            key(),
            Entry {
                hash: grammar_hash(hash),
                spec: Arc::new(spec.clone()),
                source,
            },
        );
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
            cache.rebuild_decision(
                &key(),
                &grammar_hash("abc123"),
                &spec,
                &stable_source(&spec),
            ),
            &[MissReason::MissingEntry],
        );
    }

    #[test]
    fn test_rebuild_decision_hash_mismatch() {
        let spec = test_spec();
        let cache = cache_with_entry("abc123", &spec);

        assert_miss(
            cache.rebuild_decision(
                &key(),
                &grammar_hash("def456"),
                &spec,
                &stable_source(&spec),
            ),
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
                &requested,
                &stable_source(&requested),
            ),
            &[MissReason::RefChanged {
                cached: parser::Ref::parse("v1.0.0").unwrap(),
                current: parser::Ref::parse("v2.0.0").unwrap(),
            }],
        );
    }

    #[test]
    fn test_rebuild_decision_target_changed() {
        let cached = test_spec();
        let mut requested = cached.clone();
        requested.target = args::Target::Wasm;
        let cache = cache_with_entry("abc123", &cached);

        assert_miss(
            cache.rebuild_decision(
                &key(),
                &grammar_hash("abc123"),
                &requested,
                &stable_source(&requested),
            ),
            &[MissReason::TargetChanged {
                cached: args::Target::Native,
                current: args::Target::Wasm,
            }],
        );
    }

    #[test]
    fn test_rebuild_decision_cache_hit_exact() {
        let spec = test_spec();
        let cache = cache_with_entry("abc123", &spec);

        assert_eq!(
            cache.rebuild_decision(
                &key(),
                &grammar_hash("abc123"),
                &spec,
                &stable_source(&spec)
            ),
            Decision::Hit
        );
        assert!(!cache.needs_rebuild(
            &key(),
            &grammar_hash("abc123"),
            &spec,
            &stable_source(&spec)
        ));
    }

    #[test]
    fn test_rebuild_decision_moving_source_commit_changed() {
        let spec = moving_spec();
        let cached_source = moving_source(&spec, SHA1);
        let current_source = moving_source(&spec, SHA2);
        let cache = cache_with_entry_and_source("abc123", &spec, cached_source.clone());

        assert_miss(
            cache.rebuild_decision(&key(), &grammar_hash("abc123"), &spec, &current_source),
            &[MissReason::SourceChanged {
                cached: cached_source,
                current: current_source,
            }],
        );
    }

    #[test]
    fn test_rebuild_decision_moving_source_commit_unchanged_hits() {
        let spec = moving_spec();
        let source = moving_source(&spec, SHA1);
        let cache = cache_with_entry_and_source("abc123", &spec, source.clone());

        assert_eq!(
            cache.rebuild_decision(&key(), &grammar_hash("abc123"), &spec, &source),
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
                &requested,
                &stable_source(&requested),
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
                MissReason::TargetChanged {
                    cached: args::Target::Native,
                    current: args::Target::All,
                },
            ],
        );
    }
}
