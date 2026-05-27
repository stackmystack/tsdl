use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use derive_more::derive::Display;

use crate::shutdown::Signal;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Cause(Box<Error>);

impl Cause {
    #[must_use]
    pub fn new(source: impl Into<Error>) -> Self {
        Self(Box::new(source.into()))
    }

    #[must_use]
    pub fn as_error(&self) -> &Error {
        &self.0
    }
}

impl<E> From<E> for Cause
where
    E: Into<Error>,
{
    fn from(source: E) -> Self {
        Self::new(source)
    }
}

pub trait ResultExt<T> {
    fn context(self, message: impl Into<String>) -> Result<T>;

    fn with_context(self, message: impl FnOnce() -> String) -> Result<T>;
}

impl<T, E> ResultExt<T> for std::result::Result<T, E>
where
    E: Into<Error>,
{
    fn context(self, message: impl Into<String>) -> Result<T> {
        self.map_err(|source| Error::Context {
            message: message.into(),
            source: Cause::new(source),
        })
    }

    fn with_context(self, message: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|source| Error::Context {
            message: message(),
            source: Cause::new(source),
        })
    }
}

#[derive(Debug, Display)]
pub enum ParserOp {
    #[display("Could not build in {}", dir.display())]
    Build { dir: PathBuf },
    #[display("Could not clone to {}", dir.display())]
    Clone { dir: PathBuf },
    #[display("Could not discover grammars in {}", dir.display())]
    Discover { dir: PathBuf },
    #[display("Could not copy {} to {}", src.display(), dst.display())]
    Copy { src: PathBuf, dst: PathBuf },
    #[display("Could not generate in {}", dir.display())]
    Generate { dir: PathBuf },
}

/// Main error type for tsdl operations
#[derive(Debug)]
pub enum Error {
    /// Build errors
    Build { errors: Vec<Error> },

    /// Command execution failed
    Command {
        msg: String,
        stderr: String,
        stdout: String,
    },

    /// Configuration error
    Config { message: String },

    /// Context chain (linked list of context layers)
    Context { message: String, source: Cause },

    /// Generic IO error
    Io { source: std::io::Error },

    /// Build was interrupted by a Unix signal.
    Interrupted { signal: Signal },

    /// Simple error message
    Message { message: String },

    /// Language collection failed
    LanguageCollection { related: Vec<Error> },

    /// Individual language failed
    Language { name: String, source: Cause },

    /// Specific step failed
    Step {
        name: Arc<str>,
        kind: ParserOp,
        source: Cause,
    },
}

fn format_command(
    w: &mut impl fmt::Write,
    indent: usize,
    msg: &str,
    stdout: &str,
    stderr: &str,
) -> fmt::Result {
    let prefix = " ".repeat(indent);
    write!(w, "{prefix}$ {msg}")?;

    let has_stdout = !stdout.is_empty();
    let has_stderr = !stderr.is_empty();

    if has_stdout && has_stderr {
        let mut write_section = |header: &str, content: &str| -> fmt::Result {
            writeln!(w, "\n{prefix}  {header}:")?;

            let mut lines = content.lines();
            if let Some(first) = lines.next() {
                write!(w, "{prefix}  {first}")?;
                for line in lines {
                    write!(w, "\n{prefix}  {line}")?;
                }
            }
            Ok(())
        };

        write_section("stdout", stdout)?;
        write_section("stderr", stderr)?;
    } else if has_stderr {
        writeln!(w)?;
        let mut lines = stderr.lines();
        if let Some(first) = lines.next() {
            write!(w, "{prefix}{first}")?;
            for line in lines {
                write!(w, "\n{prefix}{line}")?;
            }
        }
    } else if has_stdout {
        writeln!(w)?;
        let mut lines = stdout.lines();
        if let Some(first) = lines.next() {
            write!(w, "{prefix}{first}")?;
            for line in lines {
                write!(w, "\n{prefix}{line}")?;
            }
        }
    }

    Ok(())
}

fn format_language_collection(
    w: &mut impl fmt::Write,
    related: &[Error],
    indent: usize,
) -> fmt::Result {
    let prefix = " ".repeat(indent);
    writeln!(w, "{prefix}Could not figure out all languages:")?;

    for (i, error) in related.iter().enumerate() {
        if i > 0 {
            write!(w, ", ")?;
        }
        match error {
            Error::Language { name, .. } => write!(w, "{name}")?,
            _ => error.format(w, 0)?,
        }
    }

    Ok(())
}

fn format_build_errors(w: &mut impl fmt::Write, errors: &[Error], indent: usize) -> fmt::Result {
    let prefix = " ".repeat(indent);
    write!(w, "{prefix}Could not build all parsers.")?;

    for error in errors {
        write!(w, "\n\n")?;
        error.format(w, indent + 2)?;
    }

    Ok(())
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format(f, 0)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Build { .. }
            | Error::Command { .. }
            | Error::Config { .. }
            | Error::Interrupted { .. }
            | Error::LanguageCollection { .. }
            | Error::Message { .. } => None,
            Error::Context { source, .. }
            | Error::Language { source, .. }
            | Error::Step { source, .. } => Some(source.as_error()),
            Error::Io { source } => Some(source),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io { source }
    }
}

impl From<std::fmt::Error> for Error {
    fn from(error: std::fmt::Error) -> Self {
        Error::Message {
            message: format!("formatting error: {error}"),
        }
    }
}

impl From<std::string::FromUtf8Error> for Error {
    fn from(error: std::string::FromUtf8Error) -> Self {
        Error::Message {
            message: format!("UTF-8 conversion error: {error}"),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Error::Message {
            message: format!("HTTP request error: {error}"),
        }
    }
}

impl From<url::ParseError> for Error {
    fn from(error: url::ParseError) -> Self {
        Error::Message {
            message: format!("URL parse error: {error}"),
        }
    }
}

impl From<toml::ser::Error> for Error {
    fn from(error: toml::ser::Error) -> Self {
        Error::Message {
            message: format!("TOML serialization error: {error}"),
        }
    }
}

impl From<toml::de::Error> for Error {
    fn from(error: toml::de::Error) -> Self {
        Error::Message {
            message: format!("TOML deserialization error: {error}"),
        }
    }
}

impl From<semver::Error> for Error {
    fn from(error: semver::Error) -> Self {
        Error::Message {
            message: format!("Semver error: {error}"),
        }
    }
}

impl From<self_update::errors::Error> for Error {
    fn from(error: self_update::errors::Error) -> Self {
        Error::Message {
            message: format!("Self-update error: {error}"),
        }
    }
}

impl From<reqwest::header::InvalidHeaderValue> for Error {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Error::Message {
            message: format!("Invalid header value: {error}"),
        }
    }
}

impl From<tokio::task::JoinError> for Error {
    fn from(error: tokio::task::JoinError) -> Self {
        Error::Message {
            message: format!("Task join error: {error}"),
        }
    }
}

impl Error {
    #[must_use]
    pub fn format_indent(&self, indent: usize) -> String {
        let mut s = String::new();
        let _ = self.format(&mut s, indent);
        s
    }

    fn format(&self, w: &mut impl fmt::Write, indent: usize) -> fmt::Result {
        let prefix = " ".repeat(indent);
        match self {
            Error::Build { errors } => format_build_errors(w, errors, indent),
            Error::Command {
                msg,
                stderr,
                stdout,
            } => format_command(w, indent, msg, stdout, stderr),
            Error::Config { message } => write!(w, "{prefix}Configuration error: {message}"),
            Error::Context { message, source } => {
                write!(
                    w,
                    "{}{}\n{}",
                    prefix,
                    message,
                    source.as_error().format_indent(indent + 2)
                )
            }
            Error::Io { source } => write!(w, "{prefix}IO error: {source}"),
            Error::Interrupted { signal } => write!(w, "{prefix}Interrupted by {signal}"),
            Error::Language { name, source } => {
                write!(
                    w,
                    "{}{}\n{}",
                    prefix,
                    name,
                    source.as_error().format_indent(indent + 2)
                )
            }
            Error::LanguageCollection { related } => format_language_collection(w, related, indent),
            Error::Message { message } => write!(w, "{prefix}{message}"),
            Error::Step { name, kind, source } => {
                write!(
                    w,
                    "{}{}: {}.\n{}",
                    prefix,
                    name,
                    kind,
                    source.as_error().format_indent(indent + 4)
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_formatting_with_indentation() {
        let stderr = "remote: Repository not found.\nfatal: repository 'https://github.com/tree-sitter/tree-sitter-jsonxxx/' not found";
        let command_error = Error::Command {
            msg: "git fetch origin --depth 1 HEAD failed with exit status 128.".to_string(),
            stderr: stderr.to_string(),
            stdout: String::new(),
        };

        let step_error = Error::Step {
            name: "jsonxxx".into(),
            kind: ParserOp::Clone {
                dir: PathBuf::from(
                    "/home/firas/src/github.com/stackmystack/tsdl/tmp/tree-sitter-jsonxxx",
                ),
            },
            source: command_error.into(),
        };

        let err = Error::Build {
            errors: vec![step_error],
        };
        let formatted = err.format_indent(0);

        let expected = r"Could not build all parsers.

  jsonxxx: Could not clone to /home/firas/src/github.com/stackmystack/tsdl/tmp/tree-sitter-jsonxxx.
      $ git fetch origin --depth 1 HEAD failed with exit status 128.
      remote: Repository not found.
      fatal: repository 'https://github.com/tree-sitter/tree-sitter-jsonxxx/' not found";

        assert_eq!(formatted, expected);
    }
}
