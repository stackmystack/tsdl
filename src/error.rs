use std::path::PathBuf;

use derive_more::derive::Display;
use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Diagnostic, Error)]
#[error("{msg}\nStdOut:\n{stdout}\nStdErr:\n{stderr}")]
pub struct Command {
    pub msg: String,
    pub stderr: String,
    pub stdout: String,
}

#[derive(Debug, Diagnostic, Error)]
#[error("Could not figure out all languages")]
pub struct LanguageCollection {
    #[related]
    pub related: Vec<Language>,
}

#[derive(Debug, Error, Diagnostic)]
#[error("{name}")]
pub struct Language {
    pub name: String,
    #[source]
    #[diagnostic_source]
    pub source: Box<dyn Diagnostic + Send + Sync + 'static>,
}

#[derive(Debug, Diagnostic, Error)]
#[error("Could not build all parsers")]
pub struct Parser {
    #[related]
    pub related: Vec<Box<dyn Diagnostic + Send + Sync + 'static>>,
}

#[derive(Debug, Error, Diagnostic)]
#[error("{name}: {kind}")]
pub struct Step {
    pub name: String,
    pub kind: ParserOp,
    #[source]
    #[diagnostic_source]
    pub source: Box<dyn Diagnostic + Send + Sync + 'static>,
}

#[derive(Debug, Display)]
pub enum ParserOp {
    #[display("Could not build in {}", dir.display())]
    Build { dir: PathBuf },
    #[display("Could not clone to {}", dir.display())]
    Clone { dir: PathBuf },
    #[display("Could not copy {} to {}", src.display(), dst.display())]
    Copy { src: PathBuf, dst: PathBuf },
    #[display("Could not generate in {}", dir.display())]
    Generate { dir: PathBuf },
}
