use std::path::PathBuf;

use derive_more::derive::Display;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{msg}\nStdOut:\n{stdout}\nStdErr:\n{stderr}")]
pub struct Command {
    pub msg: String,
    pub stderr: String,
    pub stdout: String,
}

#[derive(Debug, Error)]
#[error("Could not figure out all languages:\n{}", format_languages(.related))]
pub struct LanguageCollection {
    pub related: Vec<Language>,
}

#[derive(Debug, Error)]
#[error("{name}.\n{source:?}")]
pub struct Language {
    pub name: String,
    pub source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

#[derive(Debug, Error)]
#[error("Could not build all parsers.\n{}", format_errors(.related))]
pub struct Parser {
    pub related: Vec<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

#[derive(Debug, Error)]
#[error("{name}: {kind}.\n{source:?}")]
pub struct Step {
    pub name: String,
    pub kind: ParserOp,
    pub source: Box<dyn std::error::Error + Send + Sync + 'static>,
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

fn format_languages(langs: &[Language]) -> String {
    langs
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_errors(errs: &Vec<Box<dyn std::error::Error + Send + Sync + 'static>>) -> String {
    errs.iter()
        .map(|e| format!("{e:?}"))
        .collect::<Vec<_>>()
        .join("\n")
}
