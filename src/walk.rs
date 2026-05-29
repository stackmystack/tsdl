use std::path::{Path, PathBuf};

use crate::{cache, git, shutdown, Result};

// ============================================================
// Free functions
// ============================================================

/// Collect grammar.js paths via git ls-files and compute their hashes.
pub async fn collect_grammar_paths(root: &Path) -> Result<Vec<(PathBuf, cache::GrammarHash)>> {
    let files = git::list_grammar_files(root).await?;
    let mut results = Vec::with_capacity(files.len());

    for file in files {
        shutdown::check()?;
        let full_path = root.join(&file);
        let hash = cache::hash_file(&full_path).await?;
        results.push((full_path, hash));
    }

    Ok(results)
}
