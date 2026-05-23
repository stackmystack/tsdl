use std::os::unix::process::CommandExt as _;
use std::{env, fmt::Write, os::unix::process::ExitStatusExt, process::Output};

use tokio::process::Command;
use tracing::{debug, error, info, trace};

use crate::{error, shutdown, TsdlResult};

pub trait Exec {
    fn display(&self) -> TsdlResult<String>;
    fn display_full(&self) -> TsdlResult<String>;
    fn exec(&mut self) -> impl std::future::Future<Output = TsdlResult<Output>>;
}

pub trait Script {
    fn from_str(script: &str) -> Command;
}

impl Exec for Command {
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

    #[tracing::instrument(skip(self))]
    async fn exec(&mut self) -> TsdlResult<Output> {
        let cmd_full = self.display_full()?;
        let cmd_short = self.display()?;
        trace!("{cmd_full}");

        // Check for shutdown before spawning
        shutdown::check()?;

        // Capture stdout/stderr (mimics `self.output()` which we can't use
        // because we need to spawn+race rather than spawn+wait).
        self.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Put child in its own process group so signals can target the whole
        // sub-tree (e.g. `tree-sitter build` → `node` → `cc`).
        self.as_std_mut().process_group(0);

        let child = self
            .spawn()
            .map_err(|e| error::TsdlError::context("Failed to execute command", e))?;

        let child_pid = child.id();
        debug!("spawned pid={child_pid:?} cmd={cmd_short}");

        // Register PGID for second-signal SIGKILL escalation
        if let (Some(pid), Some(s)) = (child_pid, shutdown::current()) {
            s.set_pgid(pid as i32);
            debug!("registered pgid={pid}");

        // Test hook: pause while the child is running so an external
        // SIGTERM has a guaranteed window to arrive mid-execution.
        shutdown::test_delay().await;
        }

        // Race: child completion vs. shutdown signal.
        // `child_pid` is captured before the select! so the shutdown branch
        // can kill the process group even though `child` is moved into the
        // `wait_with_output` future (which is dropped on cancellation).
        let output = tokio::select! {
            output = child.wait_with_output() => {
                debug!("child exited pid={child_pid:?}");
                output
            }
            () = shutdown::cancelled() => {
                if let Some(pid) = child_pid {
                    info!("SHUTDOWN killing pgid={pid}");
                    // Kill the entire process group (negative PID)
                    unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
                }
                return Err(error::TsdlError::message(format!(
                    "Command cancelled (shutdown): {cmd_short}"
                )));
            }
        };

        let output =
            output.map_err(|e| error::TsdlError::context("Failed to execute command", e))?;

        if output.status.success() {
            return Ok(output);
        }

        let program = self.as_std().get_program().to_string_lossy();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let msg = match output.status.code() {
            Some(code) => format!("{cmd_short} failed with exit status {code}."),
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
}

impl Script for Command {
    fn from_str(script: &str) -> Command {
        let shell = env::var("SHELL").unwrap_or_else(|_| String::from("sh"));
        let mut cmd = Command::new(shell);
        cmd.args(["-c", script]);
        cmd
    }
}
