use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, ReadBuf};
use tracing::debug;

use crate::{build::BuildSpec, consts::TSDL_CACHE_FILE, error::TsdlError, TsdlResult};

/// The build cache stored in  `<build-dir>/<TSDL_CACHE_FILE>`
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
    pub fn delete(build_dir: &Path) -> TsdlResult<()> {
        let file = build_dir.join(TSDL_CACHE_FILE);
        if file.exists() {
            fs::remove_file(&file).map_err(|e| {
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

        let contents = fs::read_to_string(&file).map_err(|e| {
            TsdlError::context(format!("Reading cache file at {}", file.display()), e)
        })?;

        toml::from_str(&contents)
            .map_err(|e| TsdlError::context(format!("Parsing cache file at {}", file.display()), e))
    }

    /// Check if a parser needs rebuilding by comparing grammar hash and build definition
    pub fn needs_rebuild(&self, name: &str, hash: &str, spec: &BuildSpec) -> bool {
        // TODO: hash and name are plain str, I'd like strong types here.
        match self.get(name) {
            None => {
                debug!("No cache entry for {}, rebuild needed", name);
                true
            }
            Some(entry) => {
                let hash_eq = entry.hash.as_ref() == hash;
                let spec_eq = entry.spec.as_ref() == spec;
                let needs_rebuild = !(hash_eq && spec_eq);

                if needs_rebuild {
                    debug!(
                        "Cache mismatch for {}: hash={} (cached={}), config_changed=true",
                        name, hash, entry.hash
                    );
                } else {
                    debug!("Cache hit for {}, no rebuild needed", name);
                }

                needs_rebuild
            }
        }
    }

    /// Save the cache to disk
    pub fn save(&self) -> TsdlResult<()> {
        let contents = toml::to_string_pretty(self)
            .map_err(|e| TsdlError::context("Serializing cache to TOML", e))?;

        fs::write(&self.file, contents).map_err(|e| {
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
    Ok(format!("{result:x}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{Target, TreeSitter};
    use crate::git::GitRef;

    #[test]
    fn test_needs_rebuild_no_entry() {
        let cache = Db::default();
        let test_definition = BuildSpec {
            build_script: None,
            git_ref: GitRef::from("master"),
            repo: "https://github.com/example/parser".parse().unwrap(),
            tree_sitter: TreeSitter::default(),
            prefix: String::new(),
            target: Target::Native,
        };
        assert!(cache.needs_rebuild("test-parser", "abc123", &test_definition));
    }

    #[test]
    fn test_needs_rebuild_sha1_mismatch() {
        let mut cache = Db::default();
        let spec = BuildSpec {
            build_script: None,
            git_ref: GitRef::from("master"),
            repo: "https://github.com/example/parser".parse().unwrap(),
            tree_sitter: TreeSitter::default(),
            prefix: String::new(),
            target: Target::All,
        };
        cache.set(
            "test-parser".to_string(),
            Entry {
                hash: "abc123".into(),
                spec: spec.into(),
            },
        );

        let current_definition = BuildSpec {
            build_script: None,
            git_ref: GitRef::from("master"),
            repo: "https://github.com/example/parser".parse().unwrap(),
            tree_sitter: TreeSitter::default(),
            prefix: String::new(),
            target: Target::Native,
        };
        assert!(cache.needs_rebuild("test-parser", "def456", &current_definition));
    }

    #[test]
    fn test_needs_rebuild_git_ref_mismatch() {
        let mut cache = Db::default();
        let spec = BuildSpec {
            build_script: None,
            git_ref: GitRef::from("master"),
            repo: "https://github.com/example/parser".parse().unwrap(),
            tree_sitter: TreeSitter::default(),
            prefix: String::new(),
            target: Target::All,
        };
        cache.set(
            "test-parser".to_string(),
            Entry {
                hash: "abc123".into(),
                spec: spec.into(),
            },
        );

        let current_definition = BuildSpec {
            build_script: None,
            git_ref: GitRef::from("v1.0.0"),
            repo: "https://github.com/example/parser".parse().unwrap(),
            tree_sitter: TreeSitter::default(),
            prefix: String::new(),
            target: Target::Native,
        };
        assert!(cache.needs_rebuild("test-parser", "abc123", &current_definition));
    }

    #[test]
    fn test_needs_rebuild_target_not_covered() {
        let mut cache = Db::default();
        let spec = BuildSpec {
            build_script: None,
            git_ref: GitRef::from("master"),
            repo: "https://github.com/example/parser".parse().unwrap(),
            tree_sitter: TreeSitter::default(),
            prefix: String::new(),
            target: Target::Native,
        };
        cache.set(
            "test-parser".to_string(),
            Entry {
                hash: "abc123".into(),
                spec: spec.into(),
            },
        );

        let current_definition = BuildSpec {
            build_script: None,
            git_ref: GitRef::from("master"),
            repo: "https://github.com/example/parser".parse().unwrap(),
            tree_sitter: TreeSitter::default(),
            prefix: String::new(),
            target: Target::Wasm,
        };
        assert!(cache.needs_rebuild("test-parser", "abc123", &current_definition));
    }

    #[test]
    fn test_needs_rebuild_cache_hit_exact() {
        let mut cache = Db::default();
        let test_definition = BuildSpec {
            build_script: None,
            git_ref: GitRef::from("master"),
            repo: "https://github.com/example/parser".parse().unwrap(),
            tree_sitter: TreeSitter::default(),
            prefix: String::new(),
            target: Target::Native,
        };
        cache.set(
            "test-parser".to_string(),
            Entry {
                hash: "abc123".into(),
                spec: Arc::new(test_definition.clone()),
            },
        );

        assert!(!cache.needs_rebuild("test-parser", "abc123", &test_definition));
    }
}
