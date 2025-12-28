use async_stream::try_stream;
use futures::{Stream, StreamExt};
use ignore::{
    gitignore::{Gitignore, GitignoreBuilder},
    overrides::Override,
    types::Types,
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

use crate::cache;

/// Holds the immutable rules for the traversal.
struct FilterContext {
    types: Types,
    overrides: Override,
}

impl FilterContext {
    fn new(root: impl AsRef<Path>) -> Self {
        use ignore::{overrides::OverrideBuilder, types::TypesBuilder};

        let mut types_builder = TypesBuilder::new();
        types_builder.add_def("js:*.js").unwrap();
        let types = types_builder.select("js").build().unwrap();

        let mut overrides_builder = OverrideBuilder::new(root);
        overrides_builder.case_insensitive(true).unwrap();
        overrides_builder
            .add("!(.github|bindings|doc|docs|examples|queries|script|scripts|test|tests)/**")
            .unwrap();
        let overrides = overrides_builder.build().unwrap();

        Self { types, overrides }
    }

    fn is_ignored(&self, path: &Path, is_dir: bool, gitignore: &Gitignore) -> bool {
        if gitignore.matched(path, is_dir).is_ignore()
            && !self.overrides.matched(path, is_dir).is_whitelist()
        {
            return true;
        }
        false
    }
}

/// Recursive async generator
fn scan_directory(
    dir: PathBuf,
    ctx: Arc<FilterContext>,
    parent_ignore: Arc<Gitignore>,
) -> impl Stream<Item = io::Result<PathBuf>> {
    try_stream! {
        // 1. Check for a local .gitignore in this folder
        // If it exists, we must create a new matcher for this scope.
        // If not, we reuse the parent's matcher (cheap pointer copy).
        let local_ignore_path = dir.join(".gitignore");
        let active_ignore = if fs::try_exists(&local_ignore_path).await.unwrap_or(false) {
            let mut builder = GitignoreBuilder::new(&dir);
            builder.add(local_ignore_path);
            // Note: In a production 'ignore' replacement, you would chain
            // the parent_ignore here. For simplicity, we just build local.
            Arc::new(builder.build().unwrap())
        } else {
            parent_ignore
        };

        // 2. Open the directory stream
        let mut read_dir = fs::read_dir(&dir).await?;

        // 3. Iterate over entries
        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            let metadata = entry.metadata().await?;
            let is_dir = metadata.is_dir();
            let is_file = metadata.is_file();

            // 4. Check Filters
            if ctx.is_ignored(&path, is_dir, &active_ignore) {
                continue;
            }

            if is_dir {
                // RECURSION:
                // We recursively call this function and yield results from the sub-stream
                let mut sub_stream = Box::pin(scan_directory(
                    path,
                    ctx.clone(),
                    active_ignore.clone()
                ));

                while let Some(result) = sub_stream.next().await {
                    yield result?;
                }
            } else if is_file
                && is_grammar_file(&path, &ctx.types) {
                    yield path;
                }
        }
    }
}

/// Check if filename is "grammar.js" AND the path is not ignored by types
fn is_grammar_file(path: &Path, types: &Types) -> bool {
    path.file_name() == Some("grammar.js".as_ref()) && !types.matched(path, false).is_ignore()
}

/// Collect grammar.js paths and compute their hashes in a single stream.
/// Returns a stream of (path, hash) tuples.
///
/// # Panics
///
///
pub fn collect_grammar_paths_with_hash(
    root: PathBuf,
) -> impl Stream<Item = io::Result<(PathBuf, String)>> {
    let ctx = Arc::new(FilterContext::new(root.clone()));

    let mut builder = GitignoreBuilder::new(&root);
    builder.add(root.join(".gitignore"));
    let root_ignore = Arc::new(
        builder
            .build()
            .unwrap_or_else(|_| panic!("gitignore builder failed")),
    );

    try_stream! {
        let mut stream = Box::pin(scan_directory(root, ctx, root_ignore));
        while let Some(path_result) = stream.next().await {
            let path = path_result?;
            let hash = cache::hash_file(&path).await.map_err(|e| {
                io::Error::other(format!("Failed to hash {}: {}", path.display(), e))
            })?;
            yield (path, hash);
        }
    }
}

/// Collect grammar.js paths via git ls-files and compute their hashes.
/// Uses git for file enumeration (truly async, avoids blocking thread pool).
pub async fn collect_grammar_paths(
    root: Arc<PathBuf>,
) -> crate::TsdlResult<Vec<(PathBuf, String)>> {
    use crate::git;

    let files = git::list_grammar_files(&root).await?;
    let mut results = Vec::new();

    for file in files {
        let full_path = root.join(&file);
        let hash = cache::hash_file(&full_path).await?;
        results.push((full_path, hash));
    }

    Ok(results)
}
