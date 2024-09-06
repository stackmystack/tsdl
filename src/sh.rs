use std::{
    env,
    fmt::Write,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::Output,
};

use miette::{IntoDiagnostic, Result};
use tokio::process::Command;
use tracing::{error, trace};

use crate::{error, relative_to_cwd};

pub trait Exec {
    fn exec(&mut self) -> impl std::future::Future<Output = Result<Output>>;
    fn display(&self) -> String;
}

pub trait Script {
    fn from_str(script: &str) -> Command;
}

impl Exec for Command {
    #[tracing::instrument(skip(self))]
    async fn exec(&mut self) -> Result<Output> {
        trace!("{}", self.display());
        let output = self.output().await.into_diagnostic()?;
        if output.status.success() {
            Ok(output)
        } else {
            let program = self.as_std().get_program().to_str().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let msg = if let Some(code) = output.status.code() {
                format!("{} failed with exit status {}.", self.display(), code)
            } else {
                format!(
                    "{} interrupted by signal {}.",
                    program,
                    output.status.signal().unwrap()
                )
            };
            error!("{msg}\nStdOut:\n{stdout}\nStdErr\n{stderr}");
            Err(error::Command {
                msg,
                stderr,
                stdout,
            }
            .into())
        }
    }

    // This is needlessly complicated, trying to minimize allocations, like grown-ups,
    // not because it's needed —I didn't even measure anything— but becauase I'm exercising my rust.
    fn display(&self) -> String {
        let program = self.as_std().get_program();
        let args = self.as_std().get_args();
        let cwd = self.as_std().get_current_dir();
        let capacity = program.len() + 1 + args.len() + 1; // + 1 for spaces
        let mut res = String::with_capacity(
            capacity
                + cwd.map_or(
                    0,
                    // + 3 = 2 brackets and a space.
                    // we always overallocate by 1 (alignment aside); see the formatting of args.
                    |a| a.to_str().unwrap().len() + 3,
                ),
        );
        if let Some(path) = cwd {
            write!(res, "[{}] ", relative_to_cwd(path).to_str().unwrap()).unwrap();
        };
        write!(res, "{} ", program.to_str().unwrap()).unwrap();
        let mut args_iter = args.enumerate();
        if let Some((_, first_arg)) = args_iter.next() {
            write!(res, "{}", first_arg.to_str().unwrap()).unwrap();
            for (_, arg) in args_iter {
                write!(res, " {}", arg.to_str().unwrap()).unwrap();
            }
        }
        res
    }
}

impl Script for Command {
    fn from_str(script: &str) -> Command {
        let shell = env::var("SHELL").unwrap_or_else(|_| String::from("sh"));
        let mut cmd = Command::new(shell);
        cmd.args(["-c", script]);
        cmd
    }
}

/// Your local hometown one-eyed which.
///
/// stdin, stdout, and stderr are ignored.
pub async fn which(prog: &str) -> Result<PathBuf> {
    let output = Command::new("which").arg(prog).exec().await?;
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

pub async fn gunzip(gz: &Path) -> Result<Output> {
    Command::new("gunzip").arg(gz).exec().await
}
