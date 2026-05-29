//! Graceful shutdown: signal handling, process group management, and test
//! delay hooks.

use std::{
    collections::HashSet,
    fmt,
    future::Future,
    num::NonZeroU32,
    result::Result as StdResult,
    sync::{Arc, Mutex},
};

use tokio::sync::watch;
use tracing::{debug, info};

use crate::{Error, Result, ResultExt};

tokio::task_local! {
    static CURRENT_SHUTDOWN: Handle;
}

// ============================================================
// Enums
// ============================================================

/// Error returned when constructing a [`PgId`] from a raw value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PgIdError {
    /// Process group ID zero would target the current process group.
    Zero,
    /// The value cannot be represented by the platform's `pid_t` type.
    OutOfRange(u32),
}

// ============================================================
// Structs
// ============================================================

/// Cooperative shutdown signal shared across build tasks.
#[derive(Clone, Debug)]
pub struct Handle {
    tx: watch::Sender<Option<Signal>>,
    rx: watch::Receiver<Option<Signal>>,
    /// Active child process group IDs. Build commands are put in their own
    /// process groups so a signal can target each command's whole subprocess tree.
    active_pgids: Arc<Mutex<HashSet<PgId>>>,
}

/// A positive Unix process group ID.
///
/// `PgId` rejects zero and values that do not fit in `libc::pid_t`, so callers
/// cannot accidentally use `killpg(0, ...)`/`kill(-0, ...)` semantics and signal
/// the current process group.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PgId(NonZeroU32);

/// Registration guard for an active child process group.
///
/// Keep this guard alive while the child process group may still contain live
/// processes. Dropping it removes the [`PgId`] from shutdown's active set, so a
/// later second signal will no longer send `SIGKILL` to that process group.
/// Drop it promptly after the child has exited and been reaped; this narrows the
/// window where a stale, OS-reused process group ID could be signalled by a
/// later shutdown escalation.
#[derive(Debug)]
pub struct PgIdGuard {
    shutdown: Handle,
    pgid: PgId,
}

/// A Unix signal that requested shutdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signal {
    pub number: i32,
    pub name: &'static str,
}

// ============================================================
// Free functions
// ============================================================

/// Wait for the shutdown signal from the current task-local handle.
pub async fn cancelled() -> Signal {
    match current() {
        Some(shutdown) => shutdown.cancelled().await,
        None => std::future::pending::<Signal>().await,
    }
}

/// Check if shutdown was requested by the current task-local handle.
pub fn check() -> Result<()> {
    current().map_or(Ok(()), |shutdown| shutdown.check())
}

/// Get the shutdown handle scoped to the current task, if any.
#[must_use]
pub fn current() -> Option<Handle> {
    CURRENT_SHUTDOWN.try_with(Clone::clone).ok()
}

/// Get the current task-local handle or a no-op default.
#[must_use]
pub fn current_or_default() -> Handle {
    current().unwrap_or_default()
}

/// Install a shutdown handle for the duration of the given future.
pub async fn scope<F>(shutdown: Handle, future: F) -> F::Output
where
    F: Future,
{
    CURRENT_SHUTDOWN.scope(shutdown, future).await
}

/// Send a Unix signal to an entire process group via `killpg`.
fn signal_process_group(pgid: PgId, signal: Signal) {
    let rc = unsafe { libc::killpg(pgid.as_pid_t(), signal.number) };
    if rc != 0 {
        debug!(
            "failed to send {} to pgid={pgid}: {}",
            signal.name,
            std::io::Error::last_os_error()
        );
    }
}

/// Test helper: when `TSDL_TEST_DELAY_MS` is set, inserts a sleep at every step
/// boundary so signal delivery can be observed deterministically.
///
/// Only active in debug/test builds; a noop in release.
#[cfg(debug_assertions)]
pub async fn test_delay() {
    if let Ok(ms) = std::env::var("TSDL_TEST_DELAY_MS")
        && let Ok(ms) = ms.parse::<u64>()
    {
        let d = std::time::Duration::from_millis(ms);
        debug!("[tsdl:test_delay] sleeping {ms}ms");
        tokio::time::sleep(d).await;
    }
}

/// Test helper that inserts a sleep at every step boundary when
/// `TSDL_TEST_DELAY_MS` is set. This is a noop in release builds.
#[cfg(not(debug_assertions))]
pub async fn test_delay() {
    // noop in release
}

// ============================================================
// Impls
// ============================================================

impl Default for Handle {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PgIdGuard {
    fn drop(&mut self) {
        self.shutdown.unregister_pgid(self.pgid);
    }
}

impl fmt::Display for PgId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.get())
    }
}

impl fmt::Display for PgIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => write!(f, "process group id cannot be zero"),
            Self::OutOfRange(value) => {
                write!(f, "process group id {value} does not fit in libc::pid_t")
            }
        }
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl Handle {
    /// Create a new shutdown handle with no signal set.
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
    pub fn cancel_with_signal(&self, signal: Signal) {
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
    pub fn register_pgid(&self, pgid: PgId) -> PgIdGuard {
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

        PgIdGuard {
            shutdown: self.clone(),
            pgid,
        }
    }

    fn unregister_pgid(&self, pgid: PgId) {
        let mut active = self
            .active_pgids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active.remove(&pgid);
    }

    /// Check whether a shutdown signal has been recorded.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.reason().is_some()
    }

    /// Return the signal that triggered shutdown, if any.
    #[must_use]
    pub fn reason(&self) -> Option<Signal> {
        *self.rx.borrow()
    }

    /// Wait until a shutdown signal is set, then return it.
    pub async fn cancelled(&self) -> Signal {
        if let Some(signal) = self.reason() {
            return signal;
        }

        let mut rx = self.rx.clone();
        loop {
            if rx.changed().await.is_err() {
                return std::future::pending::<Signal>().await;
            }
            if let Some(signal) = *rx.borrow() {
                return signal;
            }
        }
    }

    /// Return `Err(Interrupted)` if shutdown was signalled, or `Ok(())` otherwise.
    pub fn check(&self) -> Result<()> {
        if let Some(signal) = self.reason() {
            Err(Error::Interrupted { signal })
        } else {
            Ok(())
        }
    }

    /// Send a signal to every active child process group.
    pub fn signal_children(&self, signal: Signal) {
        for pgid in self.active_pgids() {
            self.signal_pgid(pgid, signal);
        }
    }

    /// Send a signal to one child process group.
    pub fn signal_pgid(&self, pgid: PgId, signal: Signal) {
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
    pub fn kill_pgid(&self, pgid: PgId) {
        info!("SHUTDOWN killing pgid={pgid}");
        signal_process_group(pgid, Signal::KILL);
    }

    /// Collect all currently registered child process group IDs.
    fn active_pgids(&self) -> Vec<PgId> {
        self.active_pgids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }

    /// Spawn a background task that listens for OS signals and triggers graceful then forceful shutdown.
    #[cfg(unix)]
    pub fn spawn_signal_listener(&self) -> Result<tokio::task::JoinHandle<()>> {
        use std::process;

        use tokio::signal::unix::{SignalKind, signal};

        let mut sighup = signal(SignalKind::from_raw(Signal::HUP.number))
            .context("Installing SIGHUP handler")?;
        let mut sigint = signal(SignalKind::from_raw(Signal::INT.number))
            .context("Installing SIGINT handler")?;
        let mut sigquit = signal(SignalKind::from_raw(Signal::QUIT.number))
            .context("Installing SIGQUIT handler")?;
        let mut sigterm = signal(SignalKind::from_raw(Signal::TERM.number))
            .context("Installing SIGTERM handler")?;
        let shutdown = self.clone();

        Ok(tokio::spawn(async move {
            let mut graceful = true;

            loop {
                let signal = tokio::select! {
                    s = sighup.recv() => s.map(|()| Signal::HUP),
                    s = sigint.recv() => s.map(|()| Signal::INT),
                    s = sigquit.recv() => s.map(|()| Signal::QUIT),
                    s = sigterm.recv() => s.map(|()| Signal::TERM),
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

impl PgId {
    /// Return the raw positive process group ID value.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0.get()
    }

    /// Convert to the platform `pid_t` type (panics if the invariant is violated).
    fn as_pid_t(self) -> libc::pid_t {
        <libc::pid_t>::try_from(self.0.get()).expect("PgId invariant: value fits in libc::pid_t")
    }
}

impl Signal {
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

impl std::error::Error for PgIdError {}

impl TryFrom<u32> for PgId {
    type Error = PgIdError;

    fn try_from(value: u32) -> StdResult<Self, Self::Error> {
        let value = NonZeroU32::new(value).ok_or(PgIdError::Zero)?;
        <libc::pid_t>::try_from(value.get()).map_err(|_| PgIdError::OutOfRange(value.get()))?;
        Ok(Self(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pgid_rejects_zero() {
        assert_eq!(PgId::try_from(0), Err(PgIdError::Zero));
    }

    #[test]
    fn pgid_accepts_positive_value() {
        let pgid = PgId::try_from(1).unwrap();
        assert_eq!(pgid.get(), 1);
    }

    #[test]
    fn pgid_rejects_values_that_do_not_fit_pid_t() {
        let too_large = u32::try_from(<libc::pid_t>::MAX)
            .unwrap_or(u32::MAX)
            .saturating_add(1);

        if <libc::pid_t>::try_from(too_large).is_err() {
            assert_eq!(
                PgId::try_from(too_large),
                Err(PgIdError::OutOfRange(too_large))
            );
        }
    }
}
