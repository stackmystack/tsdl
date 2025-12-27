use std::{collections::BTreeMap, fs, io, path::Path};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tracing::debug;

use crate::{args::Target, consts::TSDL_CACHE_FILE, error::TsdlError, TsdlResult};

/// The build cache stored in  `<build-dir>/<TSDL_CACHE_FILE>`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Cache {
    #[serde(default)]
    pub parsers: BTreeMap<String, CacheEntry>,
}

/// Cache entry for a single parser
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    /// SHA1 hash of the grammar.js file(s)
    pub grammar_sha1: String,
    /// Unix timestamp when this parser was last built
    pub timestamp: u64,
    /// Git reference that was built
    pub git_ref: String,
    /// Build target
    #[serde(default)]
    pub target: Target,
}

impl Cache {
    /// Load the cache from disk, or return the empty cache.
    pub fn load(build_dir: &Path) -> TsdlResult<Self> {
        let cache_path = build_dir.join(TSDL_CACHE_FILE);
        if !cache_path.exists() {
            debug!(
                "Cache file not found at {}, returning empty cache",
                cache_path.display()
            );
            return Ok(Cache::default());
        }

        let contents = fs::read_to_string(&cache_path).map_err(|e| {
            TsdlError::context(format!("Reading cache file at {}", cache_path.display()), e)
        })?;

        toml::from_str(&contents).map_err(|e| {
            TsdlError::context(format!("Parsing cache file at {}", cache_path.display()), e)
        })
    }

    /// Save the cache to disk
    pub fn save(&self, build_dir: &Path) -> TsdlResult<()> {
        let cache_path = build_dir.join(TSDL_CACHE_FILE);
        let contents = toml::to_string_pretty(self)
            .map_err(|e| TsdlError::context("Serializing cache to TOML", e))?;

        fs::write(&cache_path, contents).map_err(|e| {
            TsdlError::context(format!("Writing cache file to {}", cache_path.display()), e)
        })?;

        debug!("Cache saved to {}", cache_path.display());
        Ok(())
    }

    /// Get cache entry for a parser
    #[must_use]
    pub fn get(&self, parser_name: &str) -> Option<&CacheEntry> {
        self.parsers.get(parser_name)
    }

    /// Check if a parser needs rebuilding by comparing grammar SHA1 and target coverage
    pub fn needs_rebuild(
        &self,
        parser_name: &str,
        grammar_sha1: &str,
        git_ref: &str,
        requested_target: Target,
    ) -> bool {
        match self.get(parser_name) {
            None => {
                debug!("No cache entry for {}, rebuild needed", parser_name);
                true
            }
            Some(entry) => {
                let sha_matches = entry.grammar_sha1 == grammar_sha1;
                let ref_matches = entry.git_ref == git_ref;
                let target_covers = entry.target.covers(requested_target);
                let cond = !(sha_matches && ref_matches && target_covers);

                if cond {
                    debug!(
                        "Cache mismatch for {}: sha1={} (cached={}), ref={} (cached={}), target_covers={}",
                        parser_name, grammar_sha1, entry.grammar_sha1, git_ref, entry.git_ref, target_covers
                    );
                } else {
                    debug!("Cache hit for {}, no rebuild needed", parser_name);
                }

                cond
            }
        }
    }

    /// Insert or update a parser cache entry
    pub fn set(&mut self, parser_name: String, entry: CacheEntry) {
        self.parsers.insert(parser_name, entry);
    }

    /// Clear all entries
    pub fn clear(&mut self) {
        self.parsers.clear();
    }

    /// Delete the cache file from disk
    pub fn delete(build_dir: &Path) -> TsdlResult<()> {
        let cache_path = build_dir.join(TSDL_CACHE_FILE);
        if cache_path.exists() {
            fs::remove_file(&cache_path).map_err(|e| {
                TsdlError::context(
                    format!("Deleting cache file at {}", cache_path.display()),
                    e,
                )
            })?;
            debug!("Cache file deleted");
        }
        Ok(())
    }
}

/// Compute SHA1 hash of a file
pub fn sha1_file(path: &Path) -> TsdlResult<String> {
    let mut file = fs::File::open(path).map_err(|e| {
        TsdlError::context(format!("Opening file for hashing: {}", path.display()), e)
    })?;

    let mut hasher = Sha1::new();
    let mut buffer = [0; 8192];

    loop {
        let bytes_read = io::Read::read(&mut file, &mut buffer).map_err(|e| {
            TsdlError::context(format!("Reading file for hashing: {}", path.display()), e)
        })?;

        if bytes_read == 0 {
            break;
        }

        hasher.update(&buffer[..bytes_read]);
    }

    let result = hasher.finalize();
    Ok(format!("{result:x}"))
}

/// Compute SHA1 hash of a directory's grammar files
/// Returns the combined hash of all grammar.js files found (sorted by path)
pub fn sha1_grammar_dir(dir: &Path) -> TsdlResult<String> {
    let grammar_file = dir.join("grammar.js");
    if grammar_file.exists() {
        sha1_file(&grammar_file)
    } else {
        Err(TsdlError::message(format!(
            "No grammar.js found in {}",
            dir.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_covers_all_covers_native() {
        assert!(Target::All.covers(Target::Native));
    }

    #[test]
    fn test_target_covers_all_covers_wasm() {
        assert!(Target::All.covers(Target::Wasm));
    }

    #[test]
    fn test_target_covers_all_covers_all() {
        assert!(Target::All.covers(Target::All));
    }

    #[test]
    fn test_target_covers_native_covers_native() {
        assert!(Target::Native.covers(Target::Native));
    }

    #[test]
    fn test_target_covers_native_not_covers_wasm() {
        assert!(!Target::Native.covers(Target::Wasm));
    }

    #[test]
    fn test_target_covers_native_not_covers_all() {
        assert!(!Target::Native.covers(Target::All));
    }

    #[test]
    fn test_target_covers_wasm_covers_wasm() {
        assert!(Target::Wasm.covers(Target::Wasm));
    }

    #[test]
    fn test_target_covers_wasm_not_covers_native() {
        assert!(!Target::Wasm.covers(Target::Native));
    }

    #[test]
    fn test_target_covers_wasm_not_covers_all() {
        assert!(!Target::Wasm.covers(Target::All));
    }

    #[test]
    fn test_needs_rebuild_no_entry() {
        let cache = Cache::default();
        assert!(cache.needs_rebuild("test-parser", "abc123", "master", Target::Native));
    }

    #[test]
    fn test_needs_rebuild_sha1_mismatch() {
        let mut cache = Cache::default();
        cache.set(
            "test-parser".to_string(),
            CacheEntry {
                grammar_sha1: "abc123".to_string(),
                timestamp: 1_234_567_890,
                git_ref: "master".to_string(),
                target: Target::All,
            },
        );

        assert!(cache.needs_rebuild("test-parser", "def456", "master", Target::Native));
    }

    #[test]
    fn test_needs_rebuild_git_ref_mismatch() {
        let mut cache = Cache::default();
        cache.set(
            "test-parser".to_string(),
            CacheEntry {
                grammar_sha1: "abc123".to_string(),
                timestamp: 1_234_567_890,
                git_ref: "master".to_string(),
                target: Target::All,
            },
        );

        assert!(cache.needs_rebuild("test-parser", "abc123", "v1.0.0", Target::Native));
    }

    #[test]
    fn test_needs_rebuild_target_not_covered() {
        let mut cache = Cache::default();
        cache.set(
            "test-parser".to_string(),
            CacheEntry {
                grammar_sha1: "abc123".to_string(),
                timestamp: 1_234_567_890,
                git_ref: "master".to_string(),
                target: Target::Native,
            },
        );

        assert!(cache.needs_rebuild("test-parser", "abc123", "master", Target::Wasm));
    }

    #[test]
    fn test_needs_rebuild_cache_hit_all_covers_native() {
        let mut cache = Cache::default();
        cache.set(
            "test-parser".to_string(),
            CacheEntry {
                grammar_sha1: "abc123".to_string(),
                timestamp: 1_234_567_890,
                git_ref: "master".to_string(),
                target: Target::All,
            },
        );

        assert!(!cache.needs_rebuild("test-parser", "abc123", "master", Target::Native));
    }

    #[test]
    fn test_needs_rebuild_cache_hit_all_covers_wasm() {
        let mut cache = Cache::default();
        cache.set(
            "test-parser".to_string(),
            CacheEntry {
                grammar_sha1: "abc123".to_string(),
                timestamp: 1_234_567_890,
                git_ref: "master".to_string(),
                target: Target::All,
            },
        );

        assert!(!cache.needs_rebuild("test-parser", "abc123", "master", Target::Wasm));
    }

    #[test]
    fn test_needs_rebuild_cache_hit_native_exact() {
        let mut cache = Cache::default();
        cache.set(
            "test-parser".to_string(),
            CacheEntry {
                grammar_sha1: "abc123".to_string(),
                timestamp: 1_234_567_890,
                git_ref: "master".to_string(),
                target: Target::Native,
            },
        );

        assert!(!cache.needs_rebuild("test-parser", "abc123", "master", Target::Native));
    }

    #[test]
    fn test_needs_rebuild_cache_hit_wasm_exact() {
        let mut cache = Cache::default();
        cache.set(
            "test-parser".to_string(),
            CacheEntry {
                grammar_sha1: "abc123".to_string(),
                timestamp: 1_234_567_890,
                git_ref: "v1.0.0".to_string(),
                target: Target::Wasm,
            },
        );

        assert!(!cache.needs_rebuild("test-parser", "abc123", "v1.0.0", Target::Wasm));
    }
}
