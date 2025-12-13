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

/// Main error type for tsdl operations
#[derive(Debug, Error)]
pub enum TsdlError {
    /// Command execution failed
    #[error("{0}")]
    Command(#[from] Command),
    
    /// Language collection failed
    #[error("{0}")]
    LanguageCollection(#[from] LanguageCollection),
    
    /// Individual language failed
    #[error("{0}")]
    Language(#[from] Language),
    
    /// Parser building failed
    #[error("{0}")]
    Parser(#[from] Parser),
    
    /// Specific step failed
    #[error("{0}")]
    Step(#[from] Step),
    
    /// Generic IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),
    
    /// Generic error with context
    #[error("{context}: {source}")]
    Context {
        context: String,
        source: Box<dyn std::error::Error + Send + Sync + 'static>
    },
    
    /// Simple error message
    #[error("{0}")]
    Message(String),
}

impl TsdlError {
    /// Create a new error with context
    pub fn context<C, E>(context: C, source: E) -> Self
    where
        C: Into<String>,
        E: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    {
        TsdlError::Context {
            context: context.into(),
            source: source.into(),
        }
    }
    
    /// Create a simple error message
    pub fn message<M>(message: M) -> Self
    where
        M: Into<String>,
    {
        TsdlError::Message(message.into())
    }
}
