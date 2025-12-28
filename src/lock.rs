use std::{
    fs,
    path::{Path, PathBuf},
    process,
};

use sysinfo::{Pid, ProcessesToUpdate, System};
use tracing::info;

use crate::{consts::TSDL_LOCK_FILE, error::TsdlError, TsdlResult};

/// Result of checking lock status
#[derive(Debug)]
pub enum LockStatus {
    /// Lock acquired successfully
    Acquired(LockGuard),
    /// Lock exists from a different process
    LockedBy { pid: Pid, exe: String },
    /// Acquired lock is cyclic (same process)
    Cyclic,
    /// Lock exists from a stale (dead) process
    Stale(Pid),
    /// Not enough privileges to check process status
    Unknown { pid: Pid, reason: String },
}

/// A guard that holds an exclusive lock on the build directory.
/// The lock is automatically released when this guard is dropped.
#[derive(Debug)]
pub struct LockGuard {
    lock: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock);
    }
}

/// Manages lock configuration and acquisition.
pub struct Lock {
    lock_path: PathBuf,
    current_pid: Pid,
}

impl Lock {
    pub fn new(build_dir: &Path) -> Self {
        Self {
            lock_path: build_dir.join(TSDL_LOCK_FILE),
            current_pid: Pid::from(process::id() as usize),
        }
    }

    /// Check lock status and acquire if available.
    pub fn try_acquire(&self) -> TsdlResult<LockStatus> {
        if !self.lock_path.exists() {
            return self.acquire().map(LockStatus::Acquired);
        }

        self.check_existing_lock()
    }

    /// Force acquire a lock, overwriting any existing lock.
    ///
    /// This will replace any existing lock file.
    pub fn force_acquire(&self) -> TsdlResult<LockGuard> {
        self.force_unlock()?;
        self.acquire()
    }

    /// Force unlock the build directory by removing the lock file.
    ///
    /// This does not verify ownership.
    pub fn force_unlock(&self) -> TsdlResult<()> {
        if self.lock_path.exists() {
            fs::remove_file(&self.lock_path).map_err(|e| {
                TsdlError::context(
                    format!("Removing lock file {}", self.lock_path.display()),
                    e,
                )
            })?;
            info!("Lock removed from build directory");
        } else {
            info!("No lock file found");
        }

        Ok(())
    }

    /// Acquire a new lock by creating the lock file with current PID.
    fn acquire(&self) -> TsdlResult<LockGuard> {
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                TsdlError::context(format!("Creating build directory {}", parent.display()), e)
            })?;
        }

        self.write_lock_file()?;

        info!("Acquired lock on build directory");
        Ok(LockGuard {
            lock: self.lock_path.clone(),
        })
    }

    fn write_lock_file(&self) -> TsdlResult<()> {
        fs::write(&self.lock_path, self.current_pid.as_u32().to_string()).map_err(|e| {
            TsdlError::context(
                format!(
                    "Writing lock file {} with PID {}",
                    self.lock_path.display(),
                    self.current_pid
                ),
                e,
            )
        })
    }

    /// Helper for checking process status and determining lock conflicts
    fn check_existing_lock(&self) -> TsdlResult<LockStatus> {
        let lock_pid = self.read_pid_from_lock()?;

        if lock_pid == self.current_pid {
            return Ok(LockStatus::Cyclic);
        }

        // Refresh only the PIDs we care about
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::Some(&[self.current_pid, lock_pid]), true);

        match (system.process(lock_pid), system.process(self.current_pid)) {
            (Some(lock_process), Some(current_process)) => {
                match (lock_process.exe(), current_process.exe()) {
                    (Some(lock), Some(current)) if lock == current => Err(TsdlError::message(
                        format!("Build already in progress (PID {})", lock_process.pid()),
                    )),
                    (Some(lock), _) => Ok(LockStatus::LockedBy {
                        pid: lock_process.pid(),
                        exe: lock.to_string_lossy().to_string(),
                    }),
                    (None, _) => Ok(LockStatus::Unknown {
                        pid: lock_process.pid(),
                        reason: "Insufficient privileges to read process.exe".to_string(),
                    }),
                }
            }
            (None, _) => Ok(LockStatus::Stale(lock_pid)),
            (_, None) => Ok(LockStatus::Unknown {
                pid: self.current_pid,
                reason: "Insufficient privileges to read process information".to_string(),
            }),
        }
    }

    fn read_pid_from_lock(&self) -> TsdlResult<Pid> {
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
}
