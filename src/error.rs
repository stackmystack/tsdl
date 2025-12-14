use std::fmt;
use std::path::PathBuf;

use derive_more::derive::Display;

/// Macro for creating Step errors with common patterns
#[macro_export]
macro_rules! step_error {
    ($name:expr, $kind:expr, $source:expr) => {
        error::Step::new($name.to_string(), $kind, $source)
    };
}

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

impl std::error::Error for Command {}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format_with_indent(0))
    }
}

impl Command {
    pub fn format_with_indent(&self, indent: usize) -> String {
        let prefix = " ".repeat(indent);
        let mut result = format!("{}$ {}", prefix, self.msg);

        // Only show stderr/stdout if they have content
        if !self.stderr.is_empty() && !self.stdout.is_empty() {
            result.push_str(&format!(
                "\n{}  stdout:\n{}",
                prefix,
                self.stdout
                    .lines()
                    .map(|l| format!("{}  {}", prefix, l))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
            result.push_str(&format!(
                "\n{}  stderr:\n{}",
                prefix,
                self.stderr
                    .lines()
                    .map(|l| format!("{}  {}", prefix, l))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        } else if !self.stderr.is_empty() {
            result.push('\n');
            for line in self.stderr.lines() {
                result.push_str(&format!("{}{}\n", prefix, line));
            }
            result.pop(); // Remove trailing newline
        } else if !self.stdout.is_empty() {
            result.push('\n');
            for line in self.stdout.lines() {
                result.push_str(&format!("{}{}\n", prefix, line));
            }
            result.pop(); // Remove trailing newline
        }

        result
    }
}

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
    pub source: Box<TsdlError>,
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format_with_indent(0))
    }
}

impl Language {
    pub fn format_with_indent(&self, indent: usize) -> String {
        let prefix = " ".repeat(indent);
        format!(
            "{}{}\n{}",
            prefix,
            self.name,
            self.source.format_with_indent(indent + 2)
        )
    }
}

impl Language {
    pub fn new(name: String, source: impl Into<TsdlError>) -> Language {
        Language {
            name,
            source: Box::new(source.into()),
        }
    }
}

impl std::error::Error for Language {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug)]
pub struct Parser {
    pub related: Vec<TsdlError>,
}

impl fmt::Display for Parser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format_with_indent(0))
    }
}

impl Parser {
    pub fn format_with_indent(&self, indent: usize) -> String {
        let prefix = " ".repeat(indent);
        let mut result = format!("{}Could not build all parsers.", prefix);
        for err in &self.related {
            result.push_str(&format!("\n\n{}", err.format_with_indent(indent + 2)));
        }
        result
    }
}

impl std::error::Error for Parser {}

#[derive(Debug)]
pub struct Step {
    pub name: String,
    pub kind: ParserOp,
    pub source: Box<TsdlError>,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format_with_indent(0))
    }
}

impl Step {
    pub fn format_with_indent(&self, indent: usize) -> String {
        let prefix = " ".repeat(indent);
        format!(
            "{}{}: {}.\n{}",
            prefix,
            self.name,
            self.kind,
            self.source.format_with_indent(indent + 2)
        )
    }
}

impl Step {
    pub fn new(name: String, kind: ParserOp, source: impl Into<TsdlError>) -> TsdlError {
        TsdlError::Step(Step {
            name,
            kind,
            source: Box::new(source.into()),
        })
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

    /// Format the error with indentation support
    pub fn format_with_indent(&self, indent: usize) -> String {
        let prefix = " ".repeat(indent);
        match self {
            TsdlError::Command(e) => e.format_with_indent(indent),
            TsdlError::LanguageCollection(e) => format!("{}{}", prefix, e),
            TsdlError::Language(e) => e.format_with_indent(indent),
            TsdlError::Parser(e) => e.format_with_indent(indent),
            TsdlError::Step(e) => e.format_with_indent(indent),
            TsdlError::Io(e) => format!("{}IO error: {}", prefix, e),
            TsdlError::Config(msg) => format!("{}Configuration error: {}", prefix, msg),
            TsdlError::Context(kind) => {
                // For context, show message and indent the nested error
                format!(
                    "{}{}\n{}",
                    prefix,
                    kind.message,
                    self.format_context_error(&kind.error, indent + 2)
                )
            }
            TsdlError::Message(msg) => format!("{}{}", prefix, msg),
        }
    }

    fn format_context_error(&self, err: &TsdlError, indent: usize) -> String {
        err.format_with_indent(indent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_formatting_with_indentation() {
        // Simulate the jsonxxx error structure
        let stderr = "remote: Repository not found.\nfatal: repository 'https://github.com/tree-sitter/tree-sitter-jsonxxx/' not found";
        let command_error = Command {
            msg: "git fetch origin --depth 1 HEAD failed with exit status 128.".to_string(),
            stderr: stderr.to_string(),
            stdout: "".to_string(),
        };

        let step_error = Step {
            name: "jsonxxx".to_string(),
            kind: ParserOp::Clone {
                dir: PathBuf::from(
                    "/home/firas/src/github.com/stackmystack/tsdl/tmp/tree-sitter-jsonxxx",
                ),
            },
            source: Box::new(command_error.into()),
        };

        let parser_error = Parser {
            related: vec![TsdlError::Step(step_error)],
        };

        let tsdl_error = TsdlError::Parser(parser_error);
        let formatted = tsdl_error.format_with_indent(0);

        let expected = r#"Could not build all parsers.

  jsonxxx: Could not clone to /home/firas/src/github.com/stackmystack/tsdl/tmp/tree-sitter-jsonxxx.
    $ git fetch origin --depth 1 HEAD failed with exit status 128.
    remote: Repository not found.
    fatal: repository 'https://github.com/tree-sitter/tree-sitter-jsonxxx/' not found"#;

        assert_eq!(formatted, expected);
    }
}
