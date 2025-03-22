use std::{
    env::consts::DLL_EXTENSION,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, Context, Result};
use ignore::{overrides::OverrideBuilder, types::TypesBuilder, WalkBuilder};
use tokio::{fs, process::Command, sync::mpsc};
use tracing::warn;
use url::Url;

use crate::{
    display::{Handle, ProgressHandle},
    error,
    git::{clone_fast, Ref},
    sh::{Exec, Script},
    SafeCanonicalize,
};

pub const NUM_STEPS: usize = 3;

pub async fn build_languages(languages: Vec<Language>) -> Result<()> {
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
            errs.push(err.into());
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
            ts_cli,
        }
    }

    async fn process(&mut self, tx: mpsc::Sender<Result<()>>) {
        let res = self.steps().await;
        if res.is_err() {
            tx.send(res).await.unwrap();
            self.handle.err(self.git_ref.to_string());
        } else {
            self.handle.fin(self.git_ref.to_string());
            tx.send(Ok(())).await.unwrap();
        }
    }

    async fn steps(&mut self) -> Result<()> {
        self.handle.start(format!("Cloning {}", self.git_ref));
        self.clone().await?;
        self.handle.step(format!("Generating {}", self.git_ref));
        for dir in self.collect_grammars() {
            self.handle.msg(format!(
                "Generating {} in {}",
                self.git_ref,
                dir.file_name().unwrap().to_str().unwrap()
            ));
            self.build_grammar(dir).await?;
        }
        Ok(())
    }

    async fn build_grammar(&self, dir: PathBuf) -> Result<()> {
        if self.build_script.is_none() {
            self.generate(&dir).await?;
            self.handle.msg(format!(
                "Building {} parser: {}",
                self.git_ref,
                dir.file_name().unwrap().to_str().unwrap(),
            ));
        } else {
            warn!("I don't know how to generate parsers when a script/cmd is specified (it's typescript's fault)");
        }
        self.build(&dir).await?;
        self.handle.msg(format!(
            "Copying {} parser: {}",
            self.git_ref,
            dir.file_name().unwrap().to_str().unwrap(),
        ));
        self.copy(&dir).await?;
        Ok(())
    }

    async fn build(&self, dir: &Path) -> Result<()> {
        self.build_script
            .as_ref()
            .map_or_else(
                || {
                    let mut cmd = Command::new(&*self.ts_cli);
                    cmd.arg("build");
                    cmd
                },
                |script| Command::from_str(script),
            )
            .current_dir(dir)
            .exec()
            .await
            .map_err(|err| {
                error::Step {
                    name: self.name.clone(),
                    kind: error::ParserOp::Build {
                        dir: self.build_dir.clone(),
                    },
                    source: err.into(),
                }
                .into()
            })
            .and(Ok(()))
    }

    fn collect_grammars(&self) -> Vec<PathBuf> {
        let mut types_builder = TypesBuilder::new();
        types_builder.add_def("js:*.js").unwrap();
        let types = types_builder.select("js").build().unwrap();
        let mut overrides_builder = OverrideBuilder::new(&self.build_dir);
        overrides_builder.case_insensitive(true).unwrap();
        overrides_builder
            .add("!(.github|bindings|doc|docs|examples|queries|script|scripts|test|tests)/**")
            .unwrap();
        let overrides = overrides_builder.build().unwrap();
        let mut walker = WalkBuilder::new(&self.build_dir);
        walker
            .git_global(false)
            .git_ignore(true)
            .hidden(false)
            .overrides(overrides)
            .types(types);
        walker
            .build()
            .filter_map(|entry| {
                entry.ok().filter(|dir| {
                    dir.file_type().unwrap().is_file() && dir.file_name() == "grammar.js"
                })
            })
            .map(|entry| {
                entry
                    .path()
                    .to_path_buf()
                    .parent()
                    .unwrap()
                    .canon()
                    .unwrap()
            })
            .collect()
    }

    async fn copy(&self, dir: impl Into<PathBuf>) -> Result<()> {
        let dir = dir.into();
        let prefix = &self.prefix;
        let dll = self.find_dll_files(&dir).await?;
        let name = Self::extract_parser_name(&dll);
        let dst = self
            .out_dir
            .clone()
            .join(format!("{prefix}{name}.{DLL_EXTENSION}"));

        fs::copy(&dll, &dst)
            .await
            .with_context(|| format!("cp {} {}", &dll.display(), dst.display()))
            .map_err(|err| self.create_copy_error(&dll, err.to_string()).into())
            .and(Ok(()))
    }

    async fn clone(&self) -> Result<()> {
        clone_fast(self.repo.as_str(), &self.git_ref, &self.build_dir)
            .await
            .map_err(|err| {
                error::Step {
                    name: self.name.clone(),
                    kind: error::ParserOp::Clone {
                        dir: self.build_dir.clone(),
                    },
                    source: err.into(),
                }
                .into()
            })
    }

    async fn generate(&self, dir: &Path) -> Result<()> {
        Command::new(&*self.ts_cli)
            .current_dir(dir)
            .arg("generate")
            .exec()
            .await
            .map_err(|err| {
                error::Step {
                    name: self.name.clone(),
                    kind: error::ParserOp::Generate {
                        dir: self.build_dir.clone(),
                    },
                    source: err.into(),
                }
                .into()
            })
            .and(Ok(()))
    }

    async fn find_dll_files(&self, dir: &Path) -> Result<PathBuf> {
        let mut files = fs::read_dir(&dir).await.unwrap();
        let mut dlls = Vec::with_capacity(1);
        while let Ok(Some(entry)) = files.next_entry().await {
            let file_name = entry.file_name();
            let name = file_name.as_os_str().to_str().unwrap();
            if entry.file_type().await.unwrap().is_file()
                && name.ends_with(&format!(".{DLL_EXTENSION}"))
            {
                dlls.push(dir.join(name));
            }
        }
        // Error handling for no DLLs or too many DLLs
        match dlls.len() {
            0 => Err(self
                .create_copy_error(dir, format!("Couldn't find any {DLL_EXTENSION} file"))
                .into()),
            n if n > 1 => Err(self
                .create_copy_error(dir, format!("Found many {DLL_EXTENSION} files: {dlls:?}"))
                .into()),
            _ => Ok(dlls[0].clone()),
        }
    }

    fn extract_parser_name(dll_path: &Path) -> String {
        let mut name = dll_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from)
            .unwrap();
        if name == format!("parser.{DLL_EXTENSION}") {
            name = dll_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(String::from)
                .unwrap();
        }
        if name.starts_with("libtree-sitter-") {
            name = name.trim_start_matches("libtree-sitter-").to_string();
        }
        if name.ends_with(&format!(".{DLL_EXTENSION}")) {
            name = name
                .trim_end_matches(&format!(".{DLL_EXTENSION}"))
                .to_string();
        }
        name
    }

    fn create_copy_error(&self, dir: &Path, message: String) -> error::Step {
        error::Step {
            name: self.name.clone(),
            kind: error::ParserOp::Copy {
                src: self.out_dir.clone(),
                dst: dir.to_path_buf(),
            },
            source: anyhow!(message).into(),
        }
    }
}
