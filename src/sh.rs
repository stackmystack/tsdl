use std::{env, fmt::Write, os::unix::process::ExitStatusExt, process::Output};

use miette::{IntoDiagnostic, Result};
use tokio::process::Command;
use tracing::{error, trace};

use crate::{error, relative_to_cwd};

pub trait Exec {
    fn exec(&mut self) -> impl std::future::Future<Output = Result<Output>>;
    fn display(&self) -> Result<String>;
}

pub trait Script {
    fn from_str(script: &str) -> Command;
}

impl Exec for Command {
    #[tracing::instrument(skip(self))]
    async fn exec(&mut self) -> Result<Output> {
        let cmd = self.display()?;
        trace!("{}", cmd);

        let output = self.output().await.into_diagnostic()?;
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

    fn display(&self) -> Result<String> {
        let program = self.as_std().get_program().to_string_lossy();
        let args = self.as_std().get_args();
        let cwd = self.as_std().get_current_dir();
        let mut res = String::new();

        if let Some(path) = cwd {
            write!(res, "[{}] ", relative_to_cwd(path).to_string_lossy()).into_diagnostic()?;
        }

        write!(res, "{program} ").into_diagnostic()?;

        for arg in args {
            write!(res, "{} ", arg.to_string_lossy()).into_diagnostic()?;
        }

        Ok(res.trim_end().to_string())
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
