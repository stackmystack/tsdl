use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind};
use tracing::info;

use crate::{
    consts::{TSDL_LOCK_FILE, TSDL_LOG_FILE},
    error::TsdlError,
    format_duration, TsdlResult,
};

/// Information about the process currently holding the build lock.
#[derive(Debug, Clone)]
pub struct LockOwner {
    pub pid: Pid,
    pub name: String,
    pub command: Option<String>,
    pub exe: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub status: String,
    pub run_time: u64,
    pub start_time: u64,
}

impl fmt::Display for LockOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "  pid:      {}", self.pid)?;
        writeln!(f, "  process:  {}", self.name)?;
        writeln!(
            f,
            "  command:  {}",
            self.command.as_deref().unwrap_or("<unavailable>")
        )?;
        writeln!(
            f,
            "  exe:      {}",
            self.exe
                .as_ref()
                .map_or_else(|| "<unavailable>".to_string(), |p| p.display().to_string())
        )?;
        writeln!(
            f,
            "  cwd:      {}",
            self.cwd
                .as_ref()
                .map_or_else(|| "<unavailable>".to_string(), |p| p.display().to_string())
        )?;
        writeln!(
            f,
            "  runtime:  {}",
            format_duration(Duration::from_secs(self.run_time))
        )?;
        write!(f, "  status:   {}", self.status)
    }
}

/// Result of checking lock status.
#[derive(Debug)]
pub enum LockStatus {
    /// Lock acquired successfully.
    Acquired(LockGuard),
    /// Acquired lock is cyclic (same process).
    Cyclic,
    /// Lock is held by a different process.
    LockedBy(LockOwner),
    /// Lock is held, but the owner could not be identified.
    Unknown { pid: Option<Pid>, reason: String },
}

/// A guard that holds an exclusive OS lock on the build directory lock file.
///
/// The lock is released when this guard is dropped. The lock file itself is
/// intentionally left on disk because OS locks are tied to open file handles,
/// not to path existence.
#[derive(Debug)]
pub struct LockGuard {
    file: File,
    lock_path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl LockGuard {
    /// Delete every entry in the build directory except the lock file itself
    /// and the log file (which is held open by the tracing subscriber).
    ///
    /// The build directory itself is preserved; only its children are removed.
    /// This is used by `--fresh` to clean the build directory without
    /// invalidating the OS lock (which is tied to the lock file's inode).
    pub fn clear_directory(&self) -> TsdlResult<()> {
        const PROTECTED: &[&str] = &[TSDL_LOCK_FILE, TSDL_LOG_FILE];

        let build_dir = self.lock_path.parent().ok_or_else(|| {
            TsdlError::message(format!(
                "Lock path has no parent directory: {}",
                self.lock_path.display()
            ))
        })?;

        let lock_name = self
            .lock_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(TSDL_LOCK_FILE);

        for entry in fs::read_dir(build_dir).map_err(|e| {
            TsdlError::context(
                format!("Reading build directory {}", build_dir.display()),
                e,
            )
        })? {
            let entry = entry.map_err(|e| {
                TsdlError::context(
                    format!("Reading directory entry in {}", build_dir.display()),
                    e,
                )
            })?;

            let name = entry.file_name();
            let name_str = name.to_str().unwrap_or("");

            if name_str == lock_name || PROTECTED.contains(&name_str) {
                continue;
            }

            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| TsdlError::context(format!("Statting {}", path.display()), e))?;

            if file_type.is_dir() {
                fs::remove_dir_all(&path).map_err(|e| {
                    TsdlError::context(format!("Removing directory {}", path.display()), e)
                })?;
            } else {
                fs::remove_file(&path).map_err(|e| {
                    TsdlError::context(format!("Removing file {}", path.display()), e)
                })?;
            }
        }

        info!("Cleaned {}", build_dir.display());
        Ok(())
    }
}

/// Manages lock configuration and acquisition.
#[derive(Clone)]
pub struct Lock {
    current_pid: Pid,
    lock_path: PathBuf,
}

enum UnlockEvent {
    LockAcquired(Result<LockGuard, String>),
    OwnerExited,
}

impl Lock {
    #[must_use]
    pub fn new(build_dir: &Path) -> Self {
        Self {
            lock_path: build_dir.join(TSDL_LOCK_FILE),
            current_pid: Pid::from(process::id() as usize),
        }
    }

    /// Check lock status and acquire the OS lock if available.
    pub fn try_acquire(&self) -> TsdlResult<LockStatus> {
        let file = self.open_lock_file()?;

        match file.try_lock_exclusive() {
            Ok(()) => self.activate(file).map(LockStatus::Acquired),
            Err(err) if is_lock_contention(&err) => Ok(self.lock_status()),
            Err(err) => Err(TsdlError::context(
                format!("Acquiring build lock {}", self.lock_path.display()),
                err,
            )),
        }
    }

    /// Send SIGTERM to the process that held the lock when `owner` was captured.
    pub fn terminate_owner(&self, owner: &LockOwner) -> TsdlResult<()> {
        info!(
            "Sending SIGTERM to lock owner PID {} ({})",
            owner.pid, owner.name
        );
        let system = Self::system_for_pid(owner.pid);
        let process = system.process(owner.pid).ok_or_else(|| {
            TsdlError::message(format!(
                "Lock owner PID {} is no longer running; retry lock acquisition",
                owner.pid
            ))
        })?;

        if process.start_time() != owner.start_time {
            return Err(TsdlError::message(format!(
                "Refusing to send SIGTERM to PID {} because it no longer matches the process that owns the lock",
                owner.pid
            )));
        }

        match process.kill_with(Signal::Term) {
            Some(true) => {
                info!("Sent SIGTERM to lock owner PID {}", owner.pid);
                Ok(())
            }
            Some(false) => Err(TsdlError::message(format!(
                "Failed to send SIGTERM to lock owner PID {}",
                owner.pid
            ))),
            None => Err(TsdlError::message(
                "SIGTERM is not supported on this platform; cannot terminate lock owner",
            )),
        }
    }

    /// Wait for the build lock to become available after the lock owner was terminated.
    ///
    /// This races two signals: the owner process exiting and this process acquiring
    /// the OS lock. The returned guard is the proof of exclusive ownership.
    pub fn wait_for_release(&self, owner: &LockOwner, timeout: Duration) -> TsdlResult<LockGuard> {
        info!(
            "Waiting up to {} for lock release from PID {}",
            crate::format_duration(timeout),
            owner.pid
        );
        let (tx, rx) = mpsc::channel();

        let lock = self.clone();
        let lock_tx = tx.clone();
        thread::spawn(move || {
            let result = lock.wait_acquire().map_err(|err| err.to_string());
            let _ = lock_tx.send(UnlockEvent::LockAcquired(result));
        });

        let owner_for_wait = owner.clone();
        thread::spawn(move || {
            Self::wait_for_owner_exit(&owner_for_wait);
            let _ = tx.send(UnlockEvent::OwnerExited);
        });

        let deadline = Instant::now() + timeout;
        let mut owner_exited = false;

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(timeout_error(owner, timeout, owner_exited));
            }

            match rx.recv_timeout(deadline.saturating_duration_since(now)) {
                Ok(UnlockEvent::OwnerExited) => {
                    owner_exited = true;
                }
                Ok(UnlockEvent::LockAcquired(Ok(guard))) => return Ok(guard),
                Ok(UnlockEvent::LockAcquired(Err(err))) => return Err(TsdlError::message(err)),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(timeout_error(owner, timeout, owner_exited))
                }

                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(TsdlError::message(
                        "Stopped waiting for lock release because waiter threads disconnected",
                    ))
                }
            }
        }
    }

    fn activate(&self, mut file: File) -> TsdlResult<LockGuard> {
        self.write_metadata(&mut file)?;
        info!("Acquired lock on build directory");
        Ok(LockGuard {
            file,
            lock_path: self.lock_path.clone(),
        })
    }

    fn open_lock_file(&self) -> TsdlResult<File> {
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                TsdlError::context(format!("Creating build directory {}", parent.display()), e)
            })?;
        }

        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock_path)
            .map_err(|e| {
                TsdlError::context(format!("Opening lock file {}", self.lock_path.display()), e)
            })
    }

    /// Blocking acquisition used after terminating a lock owner.
    fn wait_acquire(&self) -> TsdlResult<LockGuard> {
        let file = self.open_lock_file()?;
        file.lock_exclusive().map_err(|e| {
            TsdlError::context(
                format!("Waiting for build lock {}", self.lock_path.display()),
                e,
            )
        })?;
        self.activate(file)
    }

    /// Helper for checking process status and determining lock conflicts.
    fn lock_status(&self) -> LockStatus {
        let lock_pid = match self.read_pid() {
            Ok(pid) => pid,
            Err(err) => {
                return LockStatus::Unknown {
                    pid: None,
                    reason: format!("lock is held, but owner metadata could not be read: {err}"),
                };
            }
        };

        if lock_pid == self.current_pid {
            return LockStatus::Cyclic;
        }

        match Self::owner_for_pid(lock_pid) {
            Some(owner) => LockStatus::LockedBy(owner),
            None => LockStatus::Unknown {
                pid: Some(lock_pid),
                reason: "lock is held, but the metadata PID is not running or cannot be inspected"
                    .to_string(),
            },
        }
    }

    fn owner_for_pid(pid: Pid) -> Option<LockOwner> {
        let system = Self::system_for_pid(pid);
        let process = system.process(pid)?;

        Some(LockOwner {
            pid: process.pid(),
            name: process.name().to_string_lossy().to_string(),
            command: command_line(process.cmd()),
            exe: process.exe().map(Path::to_path_buf),
            cwd: process.cwd().map(Path::to_path_buf),
            status: format!("{:?}", process.status()),
            run_time: process.run_time(),
            start_time: process.start_time(),
        })
    }

    fn read_pid(&self) -> TsdlResult<Pid> {
        let content = fs::read_to_string(&self.lock_path).map_err(|e| {
            TsdlError::context(format!("Reading lock file {}", self.lock_path.display()), e)
        })?;

        let pid: usize = content.trim().parse().map_err(|_| {
            TsdlError::message(format!(
                "Invalid PID '{}' in lock file {}",
                content.trim(),
                self.lock_path.display()
            ))
        })?;

        Ok(Pid::from(pid))
    }

    fn system_for_pid(pid: Pid) -> System {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::Always)
                .with_cwd(UpdateKind::Always)
                .with_exe(UpdateKind::Always)
                .with_user(UpdateKind::Always),
        );
        system
    }

    fn wait_for_owner_exit(owner: &LockOwner) {
        let system = Self::system_for_pid(owner.pid);
        let Some(process) = system.process(owner.pid) else {
            return;
        };

        if process.start_time() != owner.start_time {
            return;
        }

        let _ = process.wait();
    }

    fn write_metadata(&self, file: &mut File) -> TsdlResult<()> {
        file.set_len(0).map_err(|e| {
            TsdlError::context(
                format!("Truncating lock file {}", self.lock_path.display()),
                e,
            )
        })?;
        file.seek(SeekFrom::Start(0)).map_err(|e| {
            TsdlError::context(format!("Seeking lock file {}", self.lock_path.display()), e)
        })?;
        write!(file, "{}", self.current_pid.as_u32()).map_err(|e| {
            TsdlError::context(
                format!(
                    "Writing lock file {} with PID {}",
                    self.lock_path.display(),
                    self.current_pid
                ),
                e,
            )
        })?;
        file.sync_all().map_err(|e| {
            TsdlError::context(format!("Syncing lock file {}", self.lock_path.display()), e)
        })
    }
}

fn command_line(cmd: &[std::ffi::OsString]) -> Option<String> {
    if cmd.is_empty() {
        return None;
    }

    Some(
        cmd.iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn is_lock_contention(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}

fn timeout_error(owner: &LockOwner, timeout: Duration, owner_exited: bool) -> TsdlError {
    let timeout = format_duration(timeout);
    if owner_exited {
        TsdlError::message(format!(
            "PID {} exited, but tsdl did not acquire the build lock within {timeout}. \
             Another process may have acquired it first. Retry the command or use a larger \
             --unlock-timeout if shutdown is slow.",
            owner.pid
        ))
    } else {
        TsdlError::message(format!(
            "Timed out after {timeout} waiting for PID {} to exit and release the build lock. \
             The process may be ignoring SIGTERM or still shutting down. Stop it manually or \
             retry with a larger --unlock-timeout.",
            owner.pid
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::TSDL_LOCK_FILE;

    fn unused_pid() -> usize {
        let system = System::new_all();
        (1_000_000..10_000_000)
            .find(|pid| system.process(Pid::from(*pid)).is_none())
            .expect("could not find an unused PID for lock test")
    }

    #[test]
    fn stale_lock_metadata_does_not_prevent_acquiring_free_os_lock() {
        let temp = tempfile::tempdir().unwrap();
        let lock_file = temp.path().join(TSDL_LOCK_FILE);
        fs::write(&lock_file, unused_pid().to_string()).unwrap();

        let lock = Lock::new(temp.path());
        let status = lock.try_acquire().unwrap();

        assert!(
            matches!(status, LockStatus::Acquired(_)),
            "stale lock metadata should not block acquiring a free OS lock"
        );
    }
}
