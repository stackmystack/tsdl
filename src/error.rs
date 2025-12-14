use std::fmt;
use std::path::PathBuf;

use derive_more::derive::Display;

/// Represents a single layer in the context chain
#[derive(Debug)]
pub struct ContextKind {
    /// The context message
    pub message: String,
    /// The wrapped error
    pub error: TsdlError,
}

impl fmt::Display for ContextKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.message, self.error)
    }
}

#[derive(Debug)]
pub struct Command {
    pub msg: String,
    pub stderr: String,
    pub stdout: String,
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}\nStdOut:\n{}\nStdErr:\n{}",
            self.msg, self.stdout, self.stderr
        )
    }
}

impl std::error::Error for Command {}

#[derive(Debug)]
pub struct LanguageCollection {
    pub related: Vec<Language>,
}

impl fmt::Display for LanguageCollection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Could not figure out all languages:\n{}",
            format_languages(&self.related)
        )
    }
}

impl std::error::Error for LanguageCollection {}

#[derive(Debug)]
pub struct Language {
    pub name: String,
    pub source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.\n{}", self.name, self.source)
    }
}

impl std::error::Error for Language {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug)]
pub struct Parser {
    pub related: Vec<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl fmt::Display for Parser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Could not build all parsers.\n{}",
            format_errors(&self.related)
        )
    }
}

impl std::error::Error for Parser {}

#[derive(Debug)]
pub struct Step {
    pub name: String,
    pub kind: ParserOp,
    pub source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}.\n{}", self.name, self.kind, self.source)
    }
}

impl std::error::Error for Step {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
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
        .map(|e| format!("{e}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Main error type for tsdl operations
#[derive(Debug)]
pub enum TsdlError {
    /// Command execution failed
    Command(Command),
    
    /// Language collection failed
    LanguageCollection(LanguageCollection),
    
    /// Individual language failed
    Language(Language),
    
    /// Parser building failed
    Parser(Parser),
    
    /// Specific step failed
    Step(Step),
    
    /// Generic IO error
    Io(std::io::Error),
    
    /// Configuration error
    Config(String),
    
    /// Context chain (linked list of context layers)
    Context(Box<ContextKind>),
    
    /// Simple error message
    Message(String),
}

impl fmt::Display for TsdlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TsdlError::Command(e) => write!(f, "{e}"),
            TsdlError::LanguageCollection(e) => write!(f, "{e}"),
            TsdlError::Language(e) => write!(f, "{e}"),
            TsdlError::Parser(e) => write!(f, "{e}"),
            TsdlError::Step(e) => write!(f, "{e}"),
            TsdlError::Io(e) => write!(f, "IO error: {e}"),
            TsdlError::Config(msg) => write!(f, "Configuration error: {msg}"),
            TsdlError::Context(kind) => write!(f, "{kind}"),
            TsdlError::Message(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for TsdlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TsdlError::Command(e) => Some(e),
            TsdlError::LanguageCollection(e) => Some(e),
            TsdlError::Language(e) => Some(e),
            TsdlError::Parser(e) => Some(e),
            TsdlError::Step(e) => Some(e),
            TsdlError::Io(e) => Some(e),
            TsdlError::Context(kind) => Some(&kind.error),
            TsdlError::Config(_) | TsdlError::Message(_) => None,
        }
    }
}

// From trait implementations to preserve #[from] functionality
impl From<Command> for TsdlError {
    fn from(e: Command) -> Self {
        TsdlError::Command(e)
    }
}

impl From<LanguageCollection> for TsdlError {
    fn from(e: LanguageCollection) -> Self {
        TsdlError::LanguageCollection(e)
    }
}

impl From<Language> for TsdlError {
    fn from(e: Language) -> Self {
        TsdlError::Language(e)
    }
}

impl From<Parser> for TsdlError {
    fn from(e: Parser) -> Self {
        TsdlError::Parser(e)
    }
}

impl From<Step> for TsdlError {
    fn from(e: Step) -> Self {
        TsdlError::Step(e)
    }
}

impl From<std::io::Error> for TsdlError {
    fn from(e: std::io::Error) -> Self {
        TsdlError::Io(e)
    }
}

impl From<std::fmt::Error> for TsdlError {
    fn from(e: std::fmt::Error) -> Self {
        TsdlError::Message(format!("formatting error: {e}"))
    }
}

impl From<std::string::FromUtf8Error> for TsdlError {
    fn from(e: std::string::FromUtf8Error) -> Self {
        TsdlError::Message(format!("UTF-8 conversion error: {e}"))
    }
}

impl From<reqwest::Error> for TsdlError {
    fn from(e: reqwest::Error) -> Self {
        TsdlError::Message(format!("HTTP request error: {e}"))
    }
}

impl From<url::ParseError> for TsdlError {
    fn from(e: url::ParseError) -> Self {
        TsdlError::Message(format!("URL parse error: {e}"))
    }
}

impl From<toml::ser::Error> for TsdlError {
    fn from(e: toml::ser::Error) -> Self {
        TsdlError::Message(format!("TOML serialization error: {e}"))
    }
}

impl From<figment::Error> for TsdlError {
    fn from(e: figment::Error) -> Self {
        TsdlError::Message(format!("Configuration error: {e}"))
    }
}

impl From<semver::Error> for TsdlError {
    fn from(e: semver::Error) -> Self {
        TsdlError::Message(format!("Semver error: {e}"))
    }
}

impl From<self_update::errors::Error> for TsdlError {
    fn from(e: self_update::errors::Error) -> Self {
        TsdlError::Message(format!("Self-update error: {e}"))
    }
}

impl From<reqwest::header::InvalidHeaderValue> for TsdlError {
    fn from(e: reqwest::header::InvalidHeaderValue) -> Self {
        TsdlError::Message(format!("Invalid header value: {e}"))
    }
}

impl TsdlError {
    /// Wrap a TsdlError with additional context message
    /// The error parameter must be convertible to TsdlError
    pub fn context<C, E>(context: C, error: E) -> Self
    where
        C: Into<String>,
        E: Into<TsdlError>,
    {
        let message = context.into();
        let tsdl_err = error.into();
        
        // Create a context wrapper linking the message to the error
        TsdlError::Context(Box::new(ContextKind {
            message,
            error: tsdl_err,
        }))
    }
    
    /// Create a simple error message
    pub fn message<M>(message: M) -> Self
    where
        M: Into<String>,
    {
        TsdlError::Message(message.into())
    }
}
