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

use crate::{
    args::{Target, TreeSitter},
    build::BuildSpec,
    consts::TSDL_CACHE_FILE,
    error::TsdlError,
    git::GitRef,
    TsdlResult,
};

/// The build cache stored in  `[build-dir]/[TSDL_CACHE_FILE]`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Db {
    pub parsers: BTreeMap<String, Entry>,
    pub file: PathBuf,
}

/// Cache entry for a single parser
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Hash of the grammar.js file(s)
    pub hash: Arc<str>,
    /// Complete build definition that affects parser output
    pub spec: Arc<BuildSpec>,
}

/// A cache lookup result for a requested parser build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheDecision {
    Hit,
    Miss(CacheMiss),
}

/// Details explaining why a cache entry cannot satisfy a requested parser build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheMiss {
    pub reasons: Vec<CacheMissReason>,
}

/// One reason a cached parser build cannot be reused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheMissReason {
    MissingEntry,
    CacheIgnored,
    HashChanged {
        cached: Arc<str>,
        current: Arc<str>,
    },
    RepoChanged {
        cached: String,
        current: String,
    },
    GitRefChanged {
        cached: GitRef,
        current: GitRef,
    },
    TreeSitterChanged {
        cached: TreeSitter,
        current: TreeSitter,
    },
    BuildScriptChanged,
    PrefixChanged {
        cached: String,
        current: String,
    },
    TargetChanged {
        cached: Target,
        current: Target,
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

impl CacheDecision {
    #[must_use]
    pub fn miss(reason: CacheMissReason) -> Self {
        Self::Miss(CacheMiss {
            reasons: vec![reason],
        })
    }

    #[must_use]
    pub fn from_reasons(reasons: Vec<CacheMissReason>) -> Self {
        if reasons.is_empty() {
            Self::Hit
        } else {
            Self::Miss(CacheMiss { reasons })
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

impl fmt::Display for CacheDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheDecision::Hit => write!(f, "cache hit"),
            CacheDecision::Miss(miss) => write!(f, "cache miss: {miss}"),
        }
    }
}

impl CacheMiss {
    #[must_use]
    pub fn short_message(&self) -> String {
        match self.reasons.as_slice() {
            [] => "cache changed".to_string(),
            [reason] => reason.short_message().to_string(),
            reasons => {
                let labels = reasons
                    .iter()
                    .map(CacheMissReason::short_label)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("cache changed: {labels}")
            }
        }
    }
}

impl fmt::Display for CacheMiss {
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

impl CacheMissReason {
    #[must_use]
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::MissingEntry => "missing",
            Self::CacheIgnored => "ignored",
            Self::HashChanged { .. } => "hash",
            Self::RepoChanged { .. } => "repo",
            Self::GitRefChanged { .. } => "ref",
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
            Self::GitRefChanged { .. } => "git ref changed",
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

impl fmt::Display for CacheMissReason {
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
            Self::GitRefChanged { cached, current } => write!(
                f,
                "git ref changed cached={} current={}",
                cached.as_str(),
                current.as_str()
            ),
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
    pub fn rebuild_decision(&self, hash: &str, spec: &BuildSpec) -> CacheDecision {
        let mut reasons = Vec::new();
        let cached = self.spec.as_ref();

        if self.hash.as_ref() != hash {
            reasons.push(CacheMissReason::HashChanged {
                cached: self.hash.clone(),
                current: Arc::from(hash),
            });
        }

        if cached.repo != spec.repo {
            reasons.push(CacheMissReason::RepoChanged {
                cached: cached.repo.to_string(),
                current: spec.repo.to_string(),
            });
        }

        if cached.git_ref != spec.git_ref {
            reasons.push(CacheMissReason::GitRefChanged {
                cached: cached.git_ref.clone(),
                current: spec.git_ref.clone(),
            });
        }

        if cached.tree_sitter != spec.tree_sitter {
            reasons.push(CacheMissReason::TreeSitterChanged {
                cached: cached.tree_sitter.clone(),
                current: spec.tree_sitter.clone(),
            });
        }

        if cached.build_script != spec.build_script {
            reasons.push(CacheMissReason::BuildScriptChanged);
        }

        if cached.prefix != spec.prefix {
            reasons.push(CacheMissReason::PrefixChanged {
                cached: cached.prefix.clone(),
                current: spec.prefix.clone(),
            });
        }

        if cached.target != spec.target {
            reasons.push(CacheMissReason::TargetChanged {
                cached: cached.target,
                current: spec.target,
            });
        }

        CacheDecision::from_reasons(reasons)
    }
}

/// Represents a "Delta" to be applied to the cache after a successful build
#[derive(Debug, Clone)]
pub struct Update {
    pub entry: Entry,
    pub name: Arc<str>,
}

impl Db {
    /// Clear all entries
    pub fn clear(&mut self) {
        self.parsers.clear();
    }

    /// Delete the cache file from disk
    pub async fn delete(build_dir: &Path) -> TsdlResult<()> {
        let file = build_dir.join(TSDL_CACHE_FILE);
        if tokio::fs::metadata(&file).await.is_ok() {
            tokio::fs::remove_file(&file).await.map_err(|e| {
                TsdlError::context(format!("Deleting cache file at {}", file.display()), e)
            })?;
            debug!("Cache file deleted");
        }
        Ok(())
    }

    /// Get cache entry for a parser
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.parsers.get(name)
    }

    /// Load the cache from disk, or return the empty cache.
    pub fn load(build_dir: &Path) -> TsdlResult<Self> {
        let file = build_dir.join(TSDL_CACHE_FILE);
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

        let contents = std::fs::read_to_string(&file).map_err(|e| {
            TsdlError::context(format!("Reading cache file at {}", file.display()), e)
        })?;

        toml::from_str(&contents)
            .map_err(|e| TsdlError::context(format!("Parsing cache file at {}", file.display()), e))
    }

    /// Explain whether a parser cache entry can satisfy the requested build.
    pub fn rebuild_decision(&self, name: &str, hash: &str, spec: &BuildSpec) -> CacheDecision {
        // TODO: hash and name are plain str, I'd like strong types here.
        let decision = match self.get(name) {
            None => CacheDecision::miss(CacheMissReason::MissingEntry),
            Some(entry) => entry.rebuild_decision(hash, spec),
        };

        debug!("Cache decision for {name}: {decision}");
        decision
    }

    /// Check if a parser needs rebuilding by comparing grammar hash and build definition.
    #[must_use]
    pub fn needs_rebuild(&self, name: &str, hash: &str, spec: &BuildSpec) -> bool {
        self.rebuild_decision(name, hash, spec).needs_rebuild()
    }

    /// Save the cache to disk
    pub async fn save(&self) -> TsdlResult<()> {
        let contents = toml::to_string_pretty(self)
            .map_err(|e| TsdlError::context("Serializing cache to TOML", e))?;

        tokio::fs::write(&self.file, contents).await.map_err(|e| {
            TsdlError::context(format!("Writing cache file to {}", self.file.display()), e)
        })?;

        debug!("Cache saved to {}", self.file.display());
        Ok(())
    }

    /// Insert or update a parser cache entry
    pub fn set(&mut self, name: String, entry: Entry) {
        self.parsers.insert(name, entry);
    }
}

/// Hash the contents of a file using SHA-1 and return the hex string.
pub async fn hash_file(path: &Path) -> TsdlResult<String> {
    let mut file = tokio::fs::File::open(path).await.map_err(|e| {
        TsdlError::context(format!("Opening file for hashing: {}", path.display()), e)
    })?;

    let mut hasher = Sha1::new();
    let mut buffer = vec![0u8; 8192];

    loop {
        let mut read_buf = ReadBuf::new(&mut buffer);
        file.read_buf(&mut read_buf).await.map_err(|e| {
            TsdlError::context(format!("Reading file for hashing: {}", path.display()), e)
        })?;

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
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_spec() -> BuildSpec {
        BuildSpec {
            build_script: None,
            git_ref: GitRef::new("master").unwrap(),
            repo: "https://github.com/example/parser".parse().unwrap(),
            tree_sitter: TreeSitter::default(),
            prefix: String::new(),
            target: Target::Native,
        }
    }

    fn cache_with_entry(hash: &str, spec: BuildSpec) -> Db {
        let mut cache = Db::default();
        cache.set(
            "test-parser".to_string(),
            Entry {
                hash: hash.into(),
                spec: Arc::new(spec),
            },
        );
        cache
    }

    fn assert_miss(decision: CacheDecision, expected: &[CacheMissReason]) {
        match decision {
            CacheDecision::Hit => panic!("expected cache miss"),
            CacheDecision::Miss(miss) => assert_eq!(miss.reasons, expected),
        }
    }

    #[test]
    fn test_rebuild_decision_no_entry() {
        let cache = Db::default();
        let spec = test_spec();

        assert_miss(
            cache.rebuild_decision("test-parser", "abc123", &spec),
            &[CacheMissReason::MissingEntry],
        );
    }

    #[test]
    fn test_rebuild_decision_hash_mismatch() {
        let spec = test_spec();
        let cache = cache_with_entry("abc123", spec.clone());

        assert_miss(
            cache.rebuild_decision("test-parser", "def456", &spec),
            &[CacheMissReason::HashChanged {
                cached: "abc123".into(),
                current: "def456".into(),
            }],
        );
    }

    #[test]
    fn test_rebuild_decision_git_ref_mismatch() {
        let cached = test_spec();
        let mut requested = cached.clone();
        requested.git_ref = GitRef::new("v1.0.0").unwrap();
        let cache = cache_with_entry("abc123", cached);

        assert_miss(
            cache.rebuild_decision("test-parser", "abc123", &requested),
            &[CacheMissReason::GitRefChanged {
                cached: GitRef::new("master").unwrap(),
                current: GitRef::new("v1.0.0").unwrap(),
            }],
        );
    }

    #[test]
    fn test_rebuild_decision_target_changed() {
        let cached = test_spec();
        let mut requested = cached.clone();
        requested.target = Target::Wasm;
        let cache = cache_with_entry("abc123", cached);

        assert_miss(
            cache.rebuild_decision("test-parser", "abc123", &requested),
            &[CacheMissReason::TargetChanged {
                cached: Target::Native,
                current: Target::Wasm,
            }],
        );
    }

    #[test]
    fn test_rebuild_decision_cache_hit_exact() {
        let spec = test_spec();
        let cache = cache_with_entry("abc123", spec.clone());

        assert_eq!(
            cache.rebuild_decision("test-parser", "abc123", &spec),
            CacheDecision::Hit
        );
        assert!(!cache.needs_rebuild("test-parser", "abc123", &spec));
    }

    #[test]
    fn test_rebuild_decision_collects_multiple_reasons() {
        let cached = test_spec();
        let mut requested = cached.clone();
        requested.build_script = Some("make".to_string());
        requested.prefix = "custom-".to_string();
        requested.target = Target::All;
        let cache = cache_with_entry("abc123", cached);

        assert_miss(
            cache.rebuild_decision("test-parser", "def456", &requested),
            &[
                CacheMissReason::HashChanged {
                    cached: "abc123".into(),
                    current: "def456".into(),
                },
                CacheMissReason::BuildScriptChanged,
                CacheMissReason::PrefixChanged {
                    cached: String::new(),
                    current: "custom-".to_string(),
                },
                CacheMissReason::TargetChanged {
                    cached: Target::Native,
                    current: Target::All,
                },
            ],
        );
    }
}
