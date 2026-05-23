use std::future::Future;
use std::sync::{Arc, Mutex};

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

/// Cooperative shutdown signal shared across build tasks.
#[derive(Clone, Debug)]
pub struct Shutdown {
    tx: watch::Sender<bool>,
    rx: watch::Receiver<bool>,
    /// The process group ID of the most-recently-spawned build command.
    /// Set by [`Shutdown::set_pgid`] and read by the signal listener.
    pgid: Arc<Mutex<Option<i32>>>,
}

impl Shutdown {
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self {
            tx,
            rx,
            pgid: Arc::new(Mutex::new(None)),
        }
    }

    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }

    /// Store the process group ID so the signal listener can
    /// escalate to SIGKILL on a second signal.
    pub fn set_pgid(&self, pgid: i32) {
        *self
            .pgid
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pgid);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }

        let mut rx = self.rx.clone();
        loop {
            if rx.changed().await.is_err() || *rx.borrow() {
                return;
            }
        }
    }

    pub fn check(&self) -> TsdlResult<()> {
        if self.is_cancelled() {
            Err(TsdlError::message("Shutdown requested"))
        } else {
            Ok(())
        }
    }

    #[cfg(unix)]
    pub fn spawn_signal_listener(&self) -> TsdlResult<tokio::task::JoinHandle<()>> {
        use std::process;

        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|e| TsdlError::context("Installing SIGTERM handler", e))?;
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .map_err(|e| TsdlError::context("Installing SIGINT handler", e))?;
        let shutdown = self.clone();

        Ok(tokio::spawn(async move {
            let mut graceful = true;

            loop {
                let signal = tokio::select! {
                    s = sigterm.recv() => s.map(|()| "SIGTERM"),
                    s = sigint.recv() => s.map(|()| "SIGINT"),
                };

                let Some(signal) = signal else {
                    // Stream closed; tokio should restore default handlers.
                    return;
                };

                if graceful {
                    graceful = false;
                    info!("Received {signal}; stopping running build commands...");
                    shutdown.cancel();
                } else {
                    // Second signal: escalate.  Kill the process group if we
                    // know one, then force-exit.
                    info!("Received second {signal}; forcing exit.");
                    let pgid = shutdown
                        .pgid
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    if let Some(pgid) = pgid {
                        let _ = unsafe { libc::kill(-pgid, libc::SIGKILL) };
                    }
                    let code = if signal == "SIGINT" { 130 } else { 143 };
                    process::exit(code);
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

pub async fn cancelled() {
    match current() {
        Some(shutdown) => shutdown.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}
