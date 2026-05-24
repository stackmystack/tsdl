use std::{
    collections::HashSet,
    fmt,
    future::Future,
    sync::{Arc, Mutex},
};

use tokio::sync::watch;
use tracing::{debug, info};

use crate::{error::TsdlError, TsdlResult};

tokio::task_local! {
    static CURRENT_SHUTDOWN: Shutdown;
}

/// Test helper: when `TSDL_TEST_DELAY_MS` is set, inserts a sleep at every step
/// boundary so signal delivery can be observed deterministically.
///
/// Only active in debug/test builds; a noop in release.
#[cfg(debug_assertions)]
pub async fn test_delay() {
    if let Ok(ms) = std::env::var("TSDL_TEST_DELAY_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            let d = std::time::Duration::from_millis(ms);
            debug!("[tsdl:test_delay] sleeping {ms}ms");
            tokio::time::sleep(d).await;
        }
    }
}

#[cfg(not(debug_assertions))]
pub async fn test_delay() {
    // noop in release
}

/// A Unix signal that requested shutdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownSignal {
    pub number: i32,
    pub name: &'static str,
}

impl ShutdownSignal {
    pub const HUP: Self = Self {
        number: libc::SIGHUP,
        name: "SIGHUP",
    };
    pub const INT: Self = Self {
        number: libc::SIGINT,
        name: "SIGINT",
    };
    pub const QUIT: Self = Self {
        number: libc::SIGQUIT,
        name: "SIGQUIT",
    };
    pub const TERM: Self = Self {
        number: libc::SIGTERM,
        name: "SIGTERM",
    };
    pub const KILL: Self = Self {
        number: libc::SIGKILL,
        name: "SIGKILL",
    };

    /// Conventional shell exit status for commands terminated by signal N.
    #[must_use]
    pub fn shell_exit_code(self) -> u8 {
        u8::try_from(128 + self.number).unwrap_or(255)
    }
}

impl fmt::Display for ShutdownSignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// Cooperative shutdown signal shared across build tasks.
#[derive(Clone, Debug)]
pub struct Shutdown {
    tx: watch::Sender<Option<ShutdownSignal>>,
    rx: watch::Receiver<Option<ShutdownSignal>>,
    /// Active child process group IDs. Build commands are put in their own
    /// process groups so a signal can target each command's whole subprocess tree.
    active_pgids: Arc<Mutex<HashSet<i32>>>,
}

/// Registration guard for an active child process group.
///
/// Dropping the guard removes the PGID from the shutdown escalation set.
#[derive(Debug)]
pub struct PgidGuard {
    shutdown: Shutdown,
    pgid: i32,
}

impl Drop for PgidGuard {
    fn drop(&mut self) {
        self.shutdown.unregister_pgid(self.pgid);
    }
}

impl Shutdown {
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(None);
        Self {
            tx,
            rx,
            active_pgids: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Record shutdown and forward the received signal to all active children.
    pub fn cancel_with_signal(&self, signal: ShutdownSignal) {
        if self.reason().is_none() {
            let _ = self.tx.send(Some(signal));
        }
        self.signal_children(signal);
    }

    /// Register an active child process group.
    ///
    /// If shutdown was already requested, the process group is immediately sent
    /// the recorded shutdown signal so late-spawned children cannot escape it.
    #[must_use]
    pub fn register_pgid(&self, pgid: i32) -> PgidGuard {
        {
            let mut active = self
                .active_pgids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            active.insert(pgid);
        }

        if let Some(signal) = self.reason() {
            self.signal_pgid(pgid, signal);
        }

        PgidGuard {
            shutdown: self.clone(),
            pgid,
        }
    }

    fn unregister_pgid(&self, pgid: i32) {
        let mut active = self
            .active_pgids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active.remove(&pgid);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.reason().is_some()
    }

    #[must_use]
    pub fn reason(&self) -> Option<ShutdownSignal> {
        *self.rx.borrow()
    }

    pub async fn cancelled(&self) -> ShutdownSignal {
        if let Some(signal) = self.reason() {
            return signal;
        }

        let mut rx = self.rx.clone();
        loop {
            if rx.changed().await.is_err() {
                return std::future::pending::<ShutdownSignal>().await;
            }
            if let Some(signal) = *rx.borrow() {
                return signal;
            }
        }
    }

    pub fn check(&self) -> TsdlResult<()> {
        if let Some(signal) = self.reason() {
            Err(TsdlError::Interrupted(signal))
        } else {
            Ok(())
        }
    }

    /// Send a signal to every active child process group.
    pub fn signal_children(&self, signal: ShutdownSignal) {
        for pgid in self.active_pgids() {
            self.signal_pgid(pgid, signal);
        }
    }

    /// Send a signal to one child process group.
    pub fn signal_pgid(&self, pgid: i32, signal: ShutdownSignal) {
        info!("SHUTDOWN sending {} to pgid={pgid}", signal.name);
        signal_process_group(pgid, signal);
    }

    /// Send SIGKILL to every active child process group.
    pub fn kill_children(&self) {
        for pgid in self.active_pgids() {
            self.kill_pgid(pgid);
        }
    }

    /// Send SIGKILL to one child process group.
    pub fn kill_pgid(&self, pgid: i32) {
        info!("SHUTDOWN killing pgid={pgid}");
        signal_process_group(pgid, ShutdownSignal::KILL);
    }

    fn active_pgids(&self) -> Vec<i32> {
        self.active_pgids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }

    #[cfg(unix)]
    pub fn spawn_signal_listener(&self) -> TsdlResult<tokio::task::JoinHandle<()>> {
        use std::process;

        use tokio::signal::unix::{signal, SignalKind};

        let mut sighup = signal(SignalKind::from_raw(ShutdownSignal::HUP.number))
            .map_err(|e| TsdlError::context("Installing SIGHUP handler", e))?;
        let mut sigint = signal(SignalKind::from_raw(ShutdownSignal::INT.number))
            .map_err(|e| TsdlError::context("Installing SIGINT handler", e))?;
        let mut sigquit = signal(SignalKind::from_raw(ShutdownSignal::QUIT.number))
            .map_err(|e| TsdlError::context("Installing SIGQUIT handler", e))?;
        let mut sigterm = signal(SignalKind::from_raw(ShutdownSignal::TERM.number))
            .map_err(|e| TsdlError::context("Installing SIGTERM handler", e))?;
        let shutdown = self.clone();

        Ok(tokio::spawn(async move {
            let mut graceful = true;

            loop {
                let signal = tokio::select! {
                    s = sighup.recv() => s.map(|()| ShutdownSignal::HUP),
                    s = sigint.recv() => s.map(|()| ShutdownSignal::INT),
                    s = sigquit.recv() => s.map(|()| ShutdownSignal::QUIT),
                    s = sigterm.recv() => s.map(|()| ShutdownSignal::TERM),
                };

                let Some(signal) = signal else {
                    // Signal stream ended unexpectedly; stop listening.
                    return;
                };

                if graceful {
                    graceful = false;
                    info!("Received {signal}; stopping running build commands...");
                    shutdown.cancel_with_signal(signal);
                } else {
                    // Second signal: escalate. Kill every known child process group,
                    // then force-exit with the conventional signal status.
                    info!("Received second {signal}; forcing exit.");
                    shutdown.kill_children();
                    process::exit(signal.shell_exit_code().into());
                }
            }
        }))
    }
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

fn signal_process_group(pgid: i32, signal: ShutdownSignal) {
    let rc = unsafe { libc::kill(-pgid, signal.number) };
    if rc != 0 {
        debug!(
            "failed to send {} to pgid={pgid}: {}",
            signal.name,
            std::io::Error::last_os_error()
        );
    }
}

pub async fn scope<F>(shutdown: Shutdown, future: F) -> F::Output
where
    F: Future,
{
    CURRENT_SHUTDOWN.scope(shutdown, future).await
}

#[must_use]
pub fn current() -> Option<Shutdown> {
    CURRENT_SHUTDOWN.try_with(Clone::clone).ok()
}

#[must_use]
pub fn current_or_default() -> Shutdown {
    current().unwrap_or_default()
}

pub fn check() -> TsdlResult<()> {
    current().map_or(Ok(()), |shutdown| shutdown.check())
}

pub async fn cancelled() -> ShutdownSignal {
    match current() {
        Some(shutdown) => shutdown.cancelled().await,
        None => std::future::pending::<ShutdownSignal>().await,
    }
}
