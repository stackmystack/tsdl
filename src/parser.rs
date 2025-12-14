use std::{
    env::consts::DLL_EXTENSION,
    path::{Path, PathBuf},
    sync::Arc,
};

use ignore::{overrides::OverrideBuilder, types::TypesBuilder, WalkBuilder};
use tokio::{fs, process::Command, sync::mpsc};
use tracing::warn;
use url::Url;

use crate::{
    args::Target,
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
    git_ref: Ref,
    handle: ProgressHandle,
    name: String,
    out_dir: PathBuf,
    prefix: String,
    repo: Url,
    target: Target,
    ts_cli: Arc<PathBuf>,
}

impl Language {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        build_dir: PathBuf,
        build_script: Option<String>,
        git_ref: Ref,
        handle: ProgressHandle,
        name: String,
        out_dir: PathBuf,
        prefix: String,
        repo: Url,
        target: Target,
        ts_cli: Arc<PathBuf>,
    ) -> Self {
        Language {
            build_dir,
            build_script,
            git_ref,
            handle,
            name,
            out_dir,
            prefix,
            repo,
            target,
            ts_cli,
        }
    }

    async fn process(&mut self, tx: mpsc::Sender<TsdlResult<()>>) {
        let res = self.steps().await;
        if res.is_err() {
            let _ = tx.send(res).await;
            self.handle.err(self.git_ref.to_string());
        } else {
            self.handle.fin(self.git_ref.to_string());
            let _ = tx.send(Ok(())).await;
        }
    }

    async fn steps(&mut self) -> TsdlResult<()> {
        self.handle.start(format!("Cloning {}", self.git_ref));
        self.clone().await?;
        self.handle.step(format!("Generating {}", self.git_ref));

        // Wrap blocking I/O in spawn_blocking to avoid blocking the async runtime
        let build_dir = self.build_dir.clone();
        let grammars = tokio::task::spawn_blocking(move || collect_grammars(&build_dir))
            .await
            .map_err(|e| {
                TsdlError::context("Failed to collect grammars in blocking task".to_string(), e)
            })?;

        for dir in grammars {
            let dir_name = dir
                .file_name()
                .map(|n: &std::ffi::OsStr| n.to_string_lossy().to_string())
                .ok_or_else(|| {
                    TsdlError::Message(format!("Could not get dir name for {}", dir.display()))
                })?;
            self.handle
                .msg(format!("Generating {} in {}", self.git_ref, dir_name));
            self.build_grammar(dir).await?;
        }
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

        cmd.current_dir(dir).exec().await?;
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
        println!();
        println!("cp {} {}", dll.display(), dst.display());
        println!();
        fs::copy(&dll, &dst).await?;
        Ok(())
    }

    async fn clone(&self) -> TsdlResult<()> {
        clone_fast(self.repo.as_str(), &self.git_ref, &self.build_dir).await?;
        Ok(())
    }

    async fn generate(&self, dir: &Path) -> TsdlResult<()> {
        Command::new(&*self.ts_cli)
            .current_dir(dir)
            .arg("generate")
            .exec()
            .await?;
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
