use std::{env, fmt::Write, os::unix::process::ExitStatusExt, process::Output};

use tokio::process::Command;
use tracing::{error, trace};

use crate::{error, TsdlResult};

pub trait Exec {
    fn exec(&mut self) -> impl std::future::Future<Output = TsdlResult<Output>>;
    fn display(&self) -> TsdlResult<String>;
    fn display_full(&self) -> TsdlResult<String>;
}

pub trait Script {
    fn from_str(script: &str) -> Command;
}

impl Exec for Command {
    #[tracing::instrument(skip(self))]
    async fn exec(&mut self) -> TsdlResult<Output> {
        let cmd_full = self.display_full()?;
        trace!("{}", cmd_full);

        let cmd = self.display()?;
        let output = self
            .output()
            .await
            .map_err(|e| error::TsdlError::context("Failed to execute command", e))?;

        if output.status.success() {
            return Ok(output);
        }

        let program = self.as_std().get_program().to_string_lossy();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let msg = match output.status.code() {
            Some(code) => format!("{cmd} failed with exit status {code}."),
            None => format!(
                "{} interrupted by signal {}.",
                program,
                output.status.signal().unwrap()
            ),
        };

        error!("{msg}\nStdOut:\n{stdout}\nStdErr\n{stderr}");

        Err(error::Command {
            msg,
            stderr,
            stdout,
        }
        .into())
    }

    fn display(&self) -> TsdlResult<String> {
        let program = self.as_std().get_program().to_string_lossy();
        let args = self.as_std().get_args();
        let mut res = String::new();

        write!(res, "{program} ").map_err(|e| {
            error::TsdlError::context("Failed to write program to display string", e)
        })?;

        for arg in args {
            write!(res, "{} ", arg.to_string_lossy()).map_err(|e| {
                error::TsdlError::context("Failed to write argument to display string", e)
            })?;
        }

        Ok(res.trim_end().to_string())
    }

    fn display_full(&self) -> TsdlResult<String> {
        let cwd = self.as_std().get_current_dir();
        let base = self.display()?;

        match cwd {
            Some(path) => Ok(format!("[{}] {}", path.display(), base)),
            None => Ok(base),
        }
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
