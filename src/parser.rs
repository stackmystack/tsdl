use std::{
    env::consts::DLL_EXTENSION,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::{fs, process::Command};
use tracing::warn;

use crate::{
    actors::ProgressAddr,
    build::{BuildContext, BuildSpec, OutputConfig},
    cache::{Entry, Update},
    error::{self, TsdlError},
    git::clone_fast,
    sh::{Exec, Script},
    walk::collect_grammar_paths,
    TsdlResult,
};

pub const NUM_STEPS: usize = 3;
pub const WASM_EXTENSION: &str = "wasm";

/// Result message from a grammar build
#[derive(Debug, Clone)]
pub enum GrammarMessage {
    Completed(Update),
    Failed(String),
}

/// A grammar ready to be built, combining definition and cache state
#[derive(Clone, Debug)]
pub struct GrammarBuild {
    pub context: BuildContext,
    pub dir: Arc<PathBuf>,
    pub entry: Option<Entry>,
    pub hash: Arc<str>,
    pub language: Arc<str>, // Required for error reporting and cache keys; set from parent LanguageBuild
    pub name: Arc<str>,
    pub output: OutputConfig,
    pub progress: ProgressAddr, // Use language's handle
    pub spec: Arc<BuildSpec>,
    pub ts_cli: Arc<PathBuf>,
}

impl GrammarBuild {
    /// Build this grammar, returning a cache update if it was built.
    /// Uses the language's progress handle for progress reporting.
    pub async fn build(&self) -> TsdlResult<Option<Update>> {
        self.progress.step("checking cache");
        let key = format!("{}/{}", self.language, self.name);

        // Check cache: if cached and definitions match, skip build but still install
        let hit = !self.context.force && !self.needs_rebuild(&key);

        if hit {
            // Install the binary from the build directory
            if let Err(e) = self.install().await {
                self.progress.err("install");
                return Err(e);
            }

            self.progress.fin("cached");
            return Ok(None);
        }

        // Use the grammar directory path provided
        if !self.dir.exists() {
            let err = TsdlError::message(format!(
                "Grammar directory not found: {}",
                self.dir.display()
            ));
            self.progress.err(format!("{err}"));
            return Err(err);
        }

        // Build the grammar
        if let Err(e) = self.build_grammar().await {
            self.progress.err("build");
            return Err(e);
        }

        // Return cache update for this grammar
        let update = Update {
            name: key.into(),
            entry: Entry {
                hash: self.hash.clone(),
                spec: self.spec.clone(),
            },
        };

        self.progress.fin("build");

        Ok(Some(update))
    }

    fn build_command(&self, ext: &str, output_name: &str) -> Command {
        if let Some(script) = &self.spec.build_script {
            return Command::from_str(script);
        }

        let mut cmd = Command::new(self.ts_cli.as_os_str());
        cmd.arg("build");

        if ext == WASM_EXTENSION {
            cmd.arg("--wasm");
        }

        cmd.args(["--output", output_name]);
        cmd
    }

    async fn build_grammar(&self) -> TsdlResult<()> {
        // Generate parser if no custom build script
        self.progress.step("generating");
        if self.spec.build_script.is_none() {
            self.generate().await?;
        } else {
            warn!("Custom build scripts not supported for generate step (TypeScript limitation)");
        }

        // Build native and/or wasm targets
        self.progress.step("building");
        self.build_targets().await?;

        // Install built parsers
        self.progress.step("installing");
        self.install().await?;

        Ok(())
    }

    async fn build_target(&self, ext: &str) -> TsdlResult<()> {
        let output_name = self.parser_name_and_ext(ext);
        let mut cmd = self.build_command(ext, &output_name);

        cmd.current_dir(self.dir.as_ref())
            .exec()
            .await
            .map_err(|err| {
                error::TsdlError::Step(error::Step::new(
                    self.language.clone(),
                    error::ParserOp::Build {
                        dir: self.dir.to_path_buf(),
                    },
                    err,
                ))
            })?;

        Ok(())
    }

    async fn build_targets(&self) -> TsdlResult<()> {
        if self.spec.target.native() {
            self.build_target(DLL_EXTENSION).await?;
        }

        if self.spec.target.wasm() {
            self.build_target(WASM_EXTENSION).await?;
        }

        Ok(())
    }

    async fn create_hardlink(&self, src: &Path, dst: &Path) -> TsdlResult<()> {
        fs::hard_link(src, dst).await.map_err(|e| {
            TsdlError::context(format!("Linking {} -> {}", src.display(), dst.display()), e)
        })
    }

    async fn find_parser_binary(&self, ext: &str) -> TsdlResult<PathBuf> {
        let expected_name = self.parser_name_and_ext(ext);
        let mut files = fs::read_dir(self.dir.as_ref()).await.map_err(|e| {
            TsdlError::context(
                format!("Failed to read directory {}", self.dir.display()),
                e,
            )
        })?;

        let mut exact_match = None;
        let mut candidates = Vec::new();

        while let Ok(Some(entry)) = files.next_entry().await {
            if !entry.file_type().await.unwrap().is_file() {
                continue;
            }

            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();

            if name == expected_name {
                exact_match = Some(self.dir.join(&file_name));
                break;
            }

            if Path::new(&file_name).extension().and_then(|e| e.to_str()) == Some(ext) {
                candidates.push(self.dir.join(&file_name));
            }
        }

        match (exact_match, candidates.len()) {
            (Some(path), _) => Ok(path),
            (None, 0) => Err(self.missing_parser_error(ext)),
            (None, 1) => Ok(candidates.into_iter().next().unwrap()),
            (None, _) => Err(self.multiple_parsers_error(ext, &candidates)),
        }
    }

    async fn generate(&self) -> TsdlResult<()> {
        Command::new(self.ts_cli.as_os_str())
            .current_dir(self.dir.as_path())
            .arg("generate")
            .exec()
            .await
            .map(|_| ())
            .map_err(|err| {
                error::TsdlError::Step(error::Step::new(
                    self.language.clone(),
                    error::ParserOp::Generate {
                        dir: self.dir.to_path_buf(),
                    },
                    err,
                ))
            })
    }

    async fn install(&self) -> TsdlResult<()> {
        // Find and install parser binary for each extension
        if self.spec.target.native() {
            self.install_binary(DLL_EXTENSION).await?;
        }

        if self.spec.target.wasm() {
            self.install_binary(WASM_EXTENSION).await?;
        }

        Ok(())
    }

    async fn install_binary(&self, ext: &str) -> TsdlResult<()> {
        let src = self.find_parser_binary(ext).await?;
        let dst = self.output.out_dir.join(self.parser_name_and_ext(ext));

        // Check if different file exists
        if dst.exists() {
            let src_metadata = fs::metadata(&src)
                .await
                .map_err(|e| TsdlError::context(format!("Reading {}", src.display()), e))?;
            let dst_metadata = fs::metadata(&dst)
                .await
                .map_err(|e| TsdlError::context(format!("Reading {}", dst.display()), e))?;

            let src_size = src_metadata.size();
            let dst_size = dst_metadata.size();
            let src_inode = src_metadata.ino();
            let dst_inode = dst_metadata.ino();

            // Check if hardlink is broken (inodes don't match when they should)
            let hardlink_broken = src_inode != dst_inode;

            if src_size != dst_size || hardlink_broken {
                if src_size != dst_size && !self.context.force {
                    return Err(TsdlError::message(format!(
                        "Binary differs at {}. Use --force to overwrite",
                        dst.display()
                    )));
                }

                fs::remove_file(&dst)
                    .await
                    .map_err(|e| TsdlError::context(format!("Removing {}", dst.display()), e))?;

                // Report reinstallation when fixing broken hardlink
                if hardlink_broken {
                    if let Some(hnd) = self.context.progress.as_ref() {
                        hnd.msg("Reinstalled");
                    }
                }

                // Create the hardlink after removing the old one
                self.create_hardlink(&src, &dst).await?;
            } else {
                // Inodes match and sizes match - hardlink is already correct, skip
            }
        } else {
            // Destination doesn't exist, create the hardlink
            self.create_hardlink(&src, &dst).await?;
        }

        Ok(())
    }
    fn missing_parser_error(&self, ext: &str) -> TsdlError {
        error::TsdlError::Step(error::Step::new(
            self.language.clone(),
            error::ParserOp::Copy {
                src: self.output.out_dir.to_path_buf(),
                dst: self.output.build_dir.to_path_buf(),
            },
            TsdlError::message(format!("Couldn't find any {ext} file")),
        ))
    }

    fn multiple_parsers_error(&self, ext: &str, candidates: &[PathBuf]) -> TsdlError {
        error::TsdlError::Step(error::Step::new(
            self.language.clone(),
            error::ParserOp::Copy {
                src: self.output.out_dir.to_path_buf(),
                dst: self.output.build_dir.to_path_buf(),
            },
            TsdlError::message(format!("Found multiple {ext} files: {candidates:?}")),
        ))
    }

    /// Check if this grammar needs rebuilding based on cache
    fn needs_rebuild(&self, _cache_key: &str) -> bool {
        match &self.entry {
            None => true, // No cache entry - rebuild needed
            Some(entry) => {
                // Check if hash or definition changed
                let hash_eq = entry.hash == self.hash;
                let def_eq = entry.spec == self.spec;
                !(hash_eq && def_eq)
            }
        }
    }

    fn parser_name_and_ext(&self, ext: &str) -> String {
        format!("{}{}.{}", self.spec.prefix, self.name, ext)
    }
}

#[derive(Clone, Debug)]
pub struct LanguageBuild {
    pub context: BuildContext,
    pub spec: Arc<BuildSpec>,
    pub name: Arc<str>,
    pub output: OutputConfig,
}

impl LanguageBuild {
    #[must_use]
    pub fn new(
        context: BuildContext,
        spec: Arc<BuildSpec>,
        name: Arc<str>,
        output: OutputConfig,
    ) -> Self {
        Self {
            context,
            spec,
            name,
            output,
        }
    }

    pub async fn discover_grammars(&self) -> TsdlResult<Vec<(String, PathBuf, String)>> {
        let file_results = collect_grammar_paths(self.output.build_dir.clone()).await?;
        let mut grammars = Vec::new();

        for (grammar_path, hash) in file_results {
            let grammar_dir = grammar_path.parent().ok_or_else(|| {
                TsdlError::Message(format!(
                    "Could not get parent directory for {}",
                    grammar_path.display()
                ))
            })?;
            let grammar_name = extract_grammar_name(grammar_dir)?;
            grammars.push((grammar_name, grammar_dir.to_path_buf(), hash));
        }

        Ok(grammars)
    }

    pub async fn clone(&self) -> TsdlResult<()> {
        clone_fast(
            self.spec.repo.as_str(),
            &self.spec.git_ref,
            &self.output.build_dir,
        )
        .await
        .map_err(|err| {
            error::TsdlError::Step(error::Step::new(
                self.name.clone(),
                error::ParserOp::Clone {
                    dir: self.output.build_dir.to_path_buf(),
                },
                err,
            ))
        })
    }
}

fn extract_dir_name(dir: &Path) -> TsdlResult<String> {
    dir.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| TsdlError::Message(format!("Could not get dir name for {}", dir.display())))
}

/// Extract grammar name from directory (strips "tree-sitter-" prefix if present)
fn extract_grammar_name(dir: &Path) -> TsdlResult<String> {
    let dir_name = extract_dir_name(dir)?;
    let name = dir_name.strip_prefix("tree-sitter-").unwrap_or(&dir_name);
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Extract directory name from a path
    fn extract_dir_name(dir: &Path) -> TsdlResult<String> {
        dir.file_name()
            .ok_or_else(|| TsdlError::message("Could not extract directory name"))
            .map(|name| name.to_string_lossy().to_string())
    }

    /// Extract grammar name from directory (strips "tree-sitter-" prefix if present)
    fn extract_grammar_name(dir: &Path) -> TsdlResult<String> {
        let dir_name = extract_dir_name(dir)?;
        let name = dir_name.strip_prefix("tree-sitter-").unwrap_or(&dir_name);
        Ok(name.to_string())
    }

    /// Generate cache key for a grammar: "language_name/grammar_name"
    fn make_cache_key(language_name: &str, grammar_path: &Path) -> TsdlResult<String> {
        let grammar_dir = grammar_path.parent().ok_or_else(|| {
            TsdlError::Message(format!(
                "Could not get parent directory for {}",
                grammar_path.display()
            ))
        })?;

        let grammar_name = extract_grammar_name(grammar_dir)?;
        Ok(format!("{}/{}", language_name, grammar_name))
    }

    /// Parse parser name and extension
    fn parser_name_and_ext(grammar_name: &str, prefix: &str, ext: &str) -> String {
        if prefix.is_empty() {
            format!("{grammar_name}.{ext}")
        } else {
            format!("{prefix}{grammar_name}.{ext}")
        }
    }

    #[test]
    fn test_make_cache_key() {
        let path = PathBuf::from("/tmp/build/tree-sitter-typescript/grammar.js");
        let key = make_cache_key("typescript", &path).unwrap();
        assert_eq!(key, "typescript/typescript");

        let path = PathBuf::from("/tmp/build/tree-sitter-tsx/grammar.js");
        let key = make_cache_key("typescript", &path).unwrap();
        assert_eq!(key, "typescript/tsx");
    }

    #[test]
    fn test_extract_grammar_name() {
        let dir = Path::new("/tmp/build/tree-sitter-typescript");
        let name = extract_grammar_name(dir).unwrap();
        assert_eq!(name, "typescript");

        let dir = Path::new("/tmp/build/custom-parser");
        let name = extract_grammar_name(dir).unwrap();
        assert_eq!(name, "custom-parser");
    }

    #[test]
    fn test_extract_grammar_name_strips_prefix() {
        let name_with_prefix = "tree-sitter-typescript";
        let stripped = name_with_prefix
            .strip_prefix("tree-sitter-")
            .unwrap_or(name_with_prefix);
        assert_eq!(stripped, "typescript");

        let name_without_prefix = "custom-parser";
        let not_stripped = name_without_prefix
            .strip_prefix("tree-sitter-")
            .unwrap_or(name_without_prefix);
        assert_eq!(not_stripped, "custom-parser");
    }

    #[test]
    fn test_parser_name_and_ext() {
        let name = parser_name_and_ext("typescript", "", "so");
        assert_eq!(name, "typescript.so");

        let name = parser_name_and_ext("typescript", "", "wasm");
        assert_eq!(name, "typescript.wasm");
    }

    #[test]
    fn test_parser_name_with_prefix() {
        let name = parser_name_and_ext("typescript", "lib", "so");
        assert_eq!(name, "libtypescript.so");
    }

    #[tokio::test]
    async fn test_per_grammar_cache_key_format() {
        // Test that cache keys follow the "language/grammar" format
        let temp_dir = TempDir::new().unwrap();
        let grammar_dir = temp_dir.path().join("tree-sitter-tsx");
        tokio::fs::create_dir(&grammar_dir).await.unwrap();
        let grammar_file = grammar_dir.join("grammar.js");
        tokio::fs::write(&grammar_file, "module.exports = {};")
            .await
            .unwrap();

        let cache_key = make_cache_key("typescript", &grammar_file).unwrap();

        assert_eq!(cache_key, "typescript/tsx");
        assert!(
            cache_key.contains('/'),
            "Cache key should use language/grammar format"
        );
    }
}
