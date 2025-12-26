use std::{
    env::consts::DLL_EXTENSION,
    fs as std_fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ignore::{overrides::OverrideBuilder, types::TypesBuilder, WalkBuilder};
use tokio::{fs, process::Command, sync::mpsc};
use tracing::warn;
use url::Url;

use crate::{
    args::Target,
    cache::{self, CacheEntry},
    display::{Handle, ProgressHandle},
    error::{self, TsdlError},
    git::{clone_fast, Ref},
    sh::{Exec, Script},
    SafeCanonicalize, TsdlResult,
};

pub const NUM_STEPS: usize = 3;
pub const WASM_EXTENSION: &str = "wasm";

pub async fn build_languages(languages: Vec<Language>) -> TsdlResult<()> {
    let buffer = if languages.is_empty() {
        64
    } else {
        languages.len()
    };
    let (tx, mut rx) = mpsc::channel(buffer);
    for mut language in languages {
        let tx = tx.clone();
        tokio::spawn(async move {
            language.process(tx).await;
        });
    }
    drop(tx);
    let mut errs = Vec::new();
    while let Some(msg) = rx.recv().await {
        if let Err(err) = msg {
            errs.push(err);
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(error::Parser { related: errs }.into())
    }
}

#[derive(Clone, Debug)]
pub struct Language {
    build_dir: PathBuf,
    build_script: Option<String>,
    force: bool,
    git_ref: Ref,
    handle: ProgressHandle,
    name: String,
    out_dir: PathBuf,
    prefix: String,
    repo: Url,
    target: Target,
    ts_cli: Arc<PathBuf>,
    cache: Arc<Mutex<cache::Cache>>,
    cache_hit: bool,
}

impl Language {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        build_dir: PathBuf,
        build_script: Option<String>,
        force: bool,
        git_ref: Ref,
        handle: ProgressHandle,
        name: String,
        out_dir: PathBuf,
        prefix: String,
        repo: Url,
        target: Target,
        ts_cli: Arc<PathBuf>,
        cache: Arc<Mutex<cache::Cache>>,
    ) -> Self {
        Language {
            build_dir,
            build_script,
            force,
            git_ref,
            handle,
            name,
            out_dir,
            prefix,
            repo,
            target,
            ts_cli,
            cache,
            cache_hit: false,
        }
    }

    async fn process(&mut self, tx: mpsc::Sender<TsdlResult<()>>) {
        let res = self.steps().await;
        if res.is_err() {
            let _ = tx.send(res).await;
            self.handle.err(self.git_ref.to_string());
        } else {
            let msg = if self.cache_hit {
                format!("{} (cached)", self.git_ref)
            } else {
                self.git_ref.to_string()
            };
            self.handle.fin(msg);
            let _ = tx.send(Ok(())).await;
        }
    }

    async fn steps(&mut self) -> TsdlResult<()> {
        // Check cache before cloning (unless --force is set)
        let cache_hit = if self.force {
            false
        } else {
            match self.check_cache_early().await {
                Ok(hit) => hit,
                Err(e) => return Err(e),
            }
        };

        // If all grammars are cached, skip clone and build steps
        if cache_hit {
            self.cache_hit = true;
            return Ok(());
        }

        self.handle.start(format!("Cloning {}", self.git_ref));
        self.clone().await?;
        self.handle.step(format!("Generating {}", self.git_ref));

        // Wrap blocking I/O in spawn_blocking to avoid blocking the async runtime
        let build_dir = self.build_dir.clone();
        let grammars = match tokio::task::spawn_blocking(move || collect_grammars(&build_dir))
            .await
            .map_err(|e| {
                TsdlError::context("Failed to collect grammars in blocking task".to_string(), e)
            }) {
            Ok(g) => g,
            Err(e) => return Err(e),
        };

        for dir in grammars {
            let Some(dir_name) = dir
                .file_name()
                .map(|n: &std::ffi::OsStr| n.to_string_lossy().to_string())
            else {
                return Err(TsdlError::Message(format!(
                    "Could not get dir name for {}",
                    dir.display()
                )));
            };

            self.handle
                .msg(format!("Generating {} in {}", self.git_ref, dir_name));
            self.build_grammar(dir.clone()).await?;
        }

        // Update cache with grammar SHA1 after successful build
        self.update_cache_after_build().await?;

        Ok(())
    }

    async fn build_grammar(&self, dir: PathBuf) -> TsdlResult<()> {
        let dir_name = dir
            .file_name()
            .map(|n: &std::ffi::OsStr| n.to_string_lossy().to_string())
            .ok_or_else(|| {
                TsdlError::Message(format!("Could not get dir name for {}", dir.display()))
            })?;

        if self.build_script.is_none() {
            self.generate(&dir).await?;
            self.handle
                .msg(format!("Building {} parser: {}", self.git_ref, dir_name,));
        } else {
            warn!("I don't know how to generate parsers when a script/cmd is specified (it's typescript's fault)");
        }

        if self.target.native() {
            self.handle.msg(format!(
                "Building {} native parser: {}",
                self.git_ref, dir_name,
            ));
            self.build(&dir, DLL_EXTENSION).await?;
        }

        if self.target.wasm() {
            self.handle.msg(format!(
                "Building {} wasm parser: {}",
                self.git_ref, dir_name,
            ));
            self.build(&dir, WASM_EXTENSION).await?;
        }
        self.handle
            .msg(format!("Copying {} parser: {}", self.git_ref, dir_name,));
        self.copy(&dir).await?;
        Ok(())
    }

    async fn build(&self, dir: &Path, ext: &str) -> TsdlResult<()> {
        let effective_name = self.parser_name_and_ext(dir, ext)?;

        let mut cmd = if let Some(script) = &self.build_script {
            Command::from_str(script)
        } else {
            let mut cmd = Command::new(&*self.ts_cli);
            cmd.arg("build");
            if ext == WASM_EXTENSION {
                cmd.arg("--wasm");
            }
            cmd.args(["--output", &effective_name]);
            cmd
        };

        cmd.current_dir(dir).exec().await.map_err(|err| {
            error::TsdlError::Step(error::Step::new(
                self.name.clone(),
                error::ParserOp::Build {
                    dir: dir.to_path_buf(),
                },
                err,
            ))
        })?;
        Ok(())
    }

    async fn copy(&self, dir: &Path) -> TsdlResult<()> {
        if self.target.native() {
            self.do_copy(dir, DLL_EXTENSION).await?;
        }
        if self.target.wasm() {
            self.do_copy(dir, WASM_EXTENSION).await?;
        }
        Ok(())
    }

    async fn do_copy(&self, dir: &Path, ext: &str) -> TsdlResult<()> {
        let dll = self.find_dll_files(dir, ext).await?;
        let name = self.parser_name_and_ext(dir, ext)?;
        let dst = self.out_dir.clone().join(name);

        // Use hard-link installation logic
        self.install_via_hardlink(&dll, &dst).map_err(|err| {
            error::TsdlError::Step(error::Step::new(
                self.name.clone(),
                error::ParserOp::Copy {
                    src: dll.clone(),
                    dst: dst.clone(),
                },
                err,
            ))
        })?;
        Ok(())
    }

    /// Install binary via hard-link with inode checking
    fn install_via_hardlink(&self, src: &Path, dst: &Path) -> TsdlResult<()> {
        // Case 1: Destination doesn't exist → create hard-link
        if !dst.exists() {
            std_fs::hard_link(src, dst).map_err(|e| {
                TsdlError::context(
                    format!(
                        "Creating hard-link from {} to {}",
                        src.display(),
                        dst.display()
                    ),
                    e,
                )
            })?;
            self.handle.msg(format!(
                "Installed {} → {} (hard-link)",
                src.display(),
                dst.display()
            ));
            return Ok(());
        }

        // Case 2: Destination exists → check inodes
        let src_meta = std_fs::metadata(src).map_err(|e| {
            TsdlError::context(format!("Reading metadata for {}", src.display()), e)
        })?;
        let dst_meta = std_fs::metadata(dst).map_err(|e| {
            TsdlError::context(format!("Reading metadata for {}", dst.display()), e)
        })?;

        let src_ino = src_meta.ino();
        let dst_ino = dst_meta.ino();

        if src_ino == dst_ino {
            // Same inode → already installed, nothing to do
            self.handle
                .msg(format!("Already installed {} (same inode)", dst.display()));
            return Ok(());
        }

        // Different inode → binary changed or was replaced
        if self.force {
            // Remove old file and hard-link new one
            std_fs::remove_file(dst).map_err(|e| {
                TsdlError::context(format!("Removing existing {}", dst.display()), e)
            })?;
            std_fs::hard_link(src, dst).map_err(|e| {
                TsdlError::context(
                    format!(
                        "Creating hard-link from {} to {}",
                        src.display(),
                        dst.display()
                    ),
                    e,
                )
            })?;
            self.handle.msg(format!(
                "Reinstalled {} → {} (hard-link, --force)",
                src.display(),
                dst.display()
            ));
            Ok(())
        } else {
            Err(TsdlError::message(format!(
                "Binary differs at {}. Use --force to overwrite",
                dst.display()
            )))
        }
    }

    async fn clone(&self) -> TsdlResult<()> {
        clone_fast(self.repo.as_str(), &self.git_ref, &self.build_dir)
            .await
            .map_err(|err| {
                error::TsdlError::Step(error::Step::new(
                    self.name.clone(),
                    error::ParserOp::Clone {
                        dir: self.build_dir.clone(),
                    },
                    err,
                ))
            })?;
        Ok(())
    }

    async fn generate(&self, dir: &Path) -> TsdlResult<()> {
        Command::new(&*self.ts_cli)
            .current_dir(dir)
            .arg("generate")
            .exec()
            .await
            .map_err(|err| {
                error::TsdlError::Step(error::Step::new(
                    self.name.clone(),
                    error::ParserOp::Generate {
                        dir: dir.to_path_buf(),
                    },
                    err,
                ))
            })?;
        Ok(())
    }

    fn parser_name_and_ext(&self, dir: &Path, ext: &str) -> TsdlResult<String> {
        let effective_name = dir
            .file_name()
            .map(|n| {
                n.to_string_lossy()
                    .strip_prefix("tree-sitter-")
                    .map_or_else(|| n.to_string_lossy().to_string(), str::to_string)
            })
            .ok_or_else(|| {
                TsdlError::Message(format!("Could not get dir name for {}", dir.display()))
            })?;
        let prefix = &self.prefix;
        Ok(format!("{prefix}{effective_name}.{ext}"))
    }

    // Since we're generating the exact file as `prefix + name + ext` in the
    // build dir, we rely on that name to copy to output dir.

    // If that name is not present, because the user defined a user script like
    // make mostly (like in typescript), then take the first match and work
    // with that.
    async fn find_dll_files(&self, dir: &Path, ext: &str) -> TsdlResult<PathBuf> {
        let effective_name = self.parser_name_and_ext(dir, ext)?;
        let mut files = fs::read_dir(&dir).await.map_err(|e| {
            TsdlError::context(format!("Failed to read directory {}", dir.display()), e)
        })?;

        let mut exact_match = None;
        let mut all_dlls = Vec::with_capacity(1);

        while let Ok(Some(entry)) = files.next_entry().await {
            let file_name = entry.file_name();
            let name = file_name.as_os_str().to_string_lossy();
            if entry.file_type().await.unwrap().is_file() {
                if name == effective_name {
                    exact_match = Some(dir.join(&file_name));
                    break;
                } else if Path::new(&file_name)
                    .extension()
                    .and_then(|e: &std::ffi::OsStr| e.to_str())
                    == Some(ext)
                {
                    all_dlls.push(dir.join(&file_name));
                }
            }
        }

        // Error handling for no DLLs or too many DLLs
        match (exact_match, all_dlls.len()) {
            (Some(exact), _) => Ok(exact),
            (None, 0) => Err(create_copy_error(
                &self.name,
                &self.out_dir,
                dir,
                format!("Couldn't find any {ext} file"),
            )),
            (None, 1) => Ok(all_dlls[0].clone()),
            (None, _) => Err(create_copy_error(
                &self.name,
                &self.out_dir,
                dir,
                format!("Found many {ext} files: {all_dlls:?}."),
            )),
        }
    }

    /// Check cache and compute grammar hashes before cloning
    /// Returns true if cache hit (skip build), false if need to rebuild
    async fn check_cache_early(&mut self) -> TsdlResult<bool> {
        // Skip cache checks if --force is set
        if self.force {
            return Ok(false);
        }

        let build_dir = self.build_dir.clone();
        let name = self.name.clone();
        let git_ref = self.git_ref.to_string();

        // Compute grammar hashes in a blocking task
        let grammar_result =
            tokio::task::spawn_blocking(move || compute_grammar_hashes(&build_dir, &name))
                .await
                .map_err(|e| {
                    TsdlError::context(
                        "Failed to compute grammar hashes in blocking task".to_string(),
                        e,
                    )
                })?;

        // If no grammars exist in build_dir yet, can't skip (need to clone first)
        if grammar_result.is_none() {
            return Ok(false);
        }

        let (_grammar_paths, grammar_sha1) = grammar_result.unwrap();

        // Check cache
        let cache_guard = self.cache.lock().expect("cache mutex poisoned");
        Ok(!cache_guard.needs_rebuild(&self.name, &grammar_sha1, &git_ref))
    }

    /// Update cache with successful build by computing grammar SHA1
    async fn update_cache_after_build(&self) -> TsdlResult<()> {
        // Compute grammar SHA1 from the build directory
        let build_dir = self.build_dir.clone();
        let name = self.name.clone();

        let grammar_sha1 =
            tokio::task::spawn_blocking(move || compute_grammar_hashes(&build_dir, &name))
                .await
                .map_err(|e| {
                    TsdlError::context(
                        "Failed to compute grammar hash in blocking task".to_string(),
                        e,
                    )
                })?;

        if let Some((_paths, sha1)) = grammar_sha1 {
            let mut cache_guard = self.cache.lock().expect("cache mutex poisoned");
            let targets = vec![
                self.target.native().then_some(Target::Native),
                self.target.wasm().then_some(Target::Wasm),
            ]
            .into_iter()
            .flatten()
            .collect();

            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            cache_guard.set(
                self.name.clone(),
                CacheEntry {
                    grammar_sha1: sha1,
                    timestamp,
                    git_ref: self.git_ref.to_string(),
                    targets,
                },
            );
        }
        Ok(())
    }
}

/// Compute SHA1 of grammar files for a parser (before clone)
/// Returns (`grammar_paths`, `sha1_hash`) if `build_dir` exists and has grammar files, None if `build_dir` doesn't exist yet
fn compute_grammar_hashes(build_dir: &Path, _parser_name: &str) -> Option<(Vec<PathBuf>, String)> {
    // Only useful if the build_dir already exists (i.e., parser was previously built)
    if !build_dir.exists() {
        return None;
    }

    let grammars = collect_grammars(build_dir);
    if grammars.is_empty() {
        return None;
    }

    // For now, compute hash of the first grammar.js found (parser dir)
    // In future, could combine hashes if multiple grammars
    if let Some(first_grammar_dir) = grammars.first() {
        match cache::sha1_grammar_dir(first_grammar_dir) {
            Ok(sha1) => Some((grammars, sha1)),
            Err(_) => None, // Grammar file missing, force rebuild
        }
    } else {
        None
    }
}

/// Standalone function for collecting grammars to avoid lifetime issues
fn collect_grammars(build_dir: &Path) -> Vec<PathBuf> {
    let mut types_builder = TypesBuilder::new();
    types_builder.add_def("js:*.js").unwrap();
    let types = types_builder.select("js").build().unwrap();
    let mut overrides_builder = OverrideBuilder::new(build_dir);
    overrides_builder.case_insensitive(true).unwrap();
    overrides_builder
        .add("!(.github|bindings|doc|docs|examples|queries|script|scripts|test|tests)/**")
        .unwrap();
    let overrides = overrides_builder.build().unwrap();
    let mut walker = WalkBuilder::new(build_dir);
    walker
        .git_global(false)
        .git_ignore(true)
        .hidden(false)
        .overrides(overrides)
        .types(types);
    walker
        .build()
        .filter_map(|entry| {
            entry.ok().and_then(|dir| {
                if dir.file_type().unwrap().is_file() && dir.file_name() == "grammar.js" {
                    Some(dir.path().to_path_buf())
                } else {
                    None
                }
            })
        })
        .filter_map(|path| path.parent().and_then(|p| p.canon().ok()))
        .collect()
}

/// Helper function to create copy errors consistently
fn create_copy_error(name: &str, out_dir: &Path, dst_dir: &Path, message: String) -> TsdlError {
    error::TsdlError::Step(error::Step::new(
        name.to_string(),
        error::ParserOp::Copy {
            src: out_dir.to_path_buf(),
            dst: dst_dir.to_path_buf(),
        },
        TsdlError::message(message),
    ))
}
