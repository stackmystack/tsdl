use std::os::unix::process::CommandExt as _;
use std::{env, fmt::Write, os::unix::process::ExitStatusExt, process::Output, time::Duration};

use tokio::{process::Command, time};
use tracing::{debug, error, info, trace, warn};

use crate::{
    shutdown::{self, PgId},
    Error, Result, ResultExt,
};

pub trait Exec {
    fn display(&self) -> Result<String>;
    fn display_full(&self) -> Result<String>;
    fn exec(&mut self) -> impl std::future::Future<Output = Result<Output>>;
}

pub trait Script {
    fn from_str(script: &str) -> Command;
}

fn signal_display(number: i32) -> Option<String> {
    let ptr = unsafe { libc::strsignal(number) };
    if ptr.is_null() {
        None
    } else {
        Some(
            unsafe { std::ffi::CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

impl Exec for Command {
    fn display(&self) -> Result<String> {
        let program = self.as_std().get_program().to_string_lossy();
        let args = self.as_std().get_args();
        let mut res = String::new();

        write!(res, "{program} ").context("Failed to write program to display string")?;

        for arg in args {
            write!(res, "{} ", arg.to_string_lossy())
                .context("Failed to write argument to display string")?;
        }

        Ok(res.trim_end().to_string())
    }

    fn display_full(&self) -> Result<String> {
        let cwd = self.as_std().get_current_dir();
        let base = self.display()?;

        match cwd {
            Some(path) => Ok(format!("[{}] {}", path.display(), base)),
            None => Ok(base),
        }
    }

    #[tracing::instrument(skip(self))]
    async fn exec(&mut self) -> Result<Output> {
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

        let child = self.spawn().context("Failed to execute command")?;

        let child_pid = child.id();
        let pgid = child_pid
            .map(PgId::try_from)
            .transpose()
            .map_err(|e| Error::Message {
                message: format!("Invalid child process group id: {e}"),
            })?;
        debug!("spawned pid={child_pid:?} cmd={cmd_short}");

        let pgid_guard = match (pgid, shutdown::current()) {
            (Some(pgid), Some(shutdown)) => {
                debug!("registered pgid={pgid}");
                Some(shutdown.register_pgid(pgid))
            }
            _ => None,
        };

        // Test hook: pause while the child is running so an external
        // shutdown signal has a guaranteed window to arrive mid-execution.
        shutdown::test_delay().await;

        // Race: child completion vs. shutdown signal. The wait future is kept
        // alive after cancellation so the child can be reaped before returning.
        let wait = child.wait_with_output();
        tokio::pin!(wait);

        let output = tokio::select! {
            output = &mut wait => {
                debug!("child exited pid={child_pid:?}");
                output
            }
            signal = shutdown::cancelled() => {
                let grace = Duration::from_secs(5);
                info!("Command interrupted by {signal}; waiting up to {grace:?} for pid={child_pid:?}: {cmd_short}");
                match time::timeout(grace, &mut wait).await {
                    Ok(Ok(_)) => {
                        debug!("child exited after {signal} pid={child_pid:?}");
                    }
                    Ok(Err(err)) => {
                        debug!("child wait after {signal} failed pid={child_pid:?}: {err}");
                    }
                    Err(_) => {
                        if let (Some(pgid), Some(shutdown)) = (pgid, shutdown::current()) {
                            shutdown.kill_pgid(pgid);

                            let kill_grace = Duration::from_secs(2);
                            match time::timeout(kill_grace, &mut wait).await {
                                Ok(Ok(_)) => {
                                    debug!("child reaped after SIGKILL pid={child_pid:?}");
                                }
                                Ok(Err(err)) => {
                                    debug!("child wait after SIGKILL failed pid={child_pid:?}: {err}");
                                }
                                Err(_) => {
                                    warn!(
                                        "Timed out waiting {kill_grace:?} to reap pid={child_pid:?} after SIGKILL; returning interruption"
                                    );
                                }
                            }
                        } else {
                            warn!("Timed out waiting for pid={child_pid:?}, but no process group was registered; returning interruption");
                        }
                    }
                }

                // If a second signal arrives before we drop the pgid_guard,
                // `kill_children()` could target a stale PGID. PGID reuse in such a
                // tiny window is rare, but this is exactly the kind of edge case
                // process-group code tries to avoid.
                drop(pgid_guard);
                return Err(Error::Interrupted { signal });
            }
        };

        drop(pgid_guard);

        let output = output.context("Failed to execute command")?;

        if output.status.success() {
            return Ok(output);
        }

        let program = self.as_std().get_program().to_string_lossy();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        let msg = if let Some(code) = output.status.code() {
            format!("{cmd_short} failed with exit status {code}.")
        } else {
            let sig = signal_display(output.status.signal().expect("a proper signal code"))
                .unwrap_or_else(|| "UNKNOWN".to_string());
            format!("{program} interrupted by signal {sig}.")
        };

        error!("{msg}\nStdOut:\n{stdout}\nStdErr\n{stderr}");

        Err(Error::Command {
            msg,
            stderr,
            stdout,
        })
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
