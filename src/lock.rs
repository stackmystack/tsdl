//! PID-based filesystem lock to prevent concurrent builds.

use std::{
    collections::HashSet,
    ffi::OsString,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process,
    result::Result as StdResult,
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind};
use tracing::info;

use crate::{
    Error, Result, ResultExt, absolute_normalize, build::BuildDir, consts, format_duration,
};

// ============================================================
// Enums
// ============================================================

/// Last observed state while waiting for a lock takeover to complete.
#[derive(Debug, Clone)]
pub enum Observation {
    /// The lock is still held by an identifiable owner.
    LockedBy(Box<Owner>),
    /// The lock is still held, but owner metadata could not be resolved.
    Unknown { pid: Option<Pid>, reason: String },
}

/// Result of checking lock status.
#[derive(Debug)]
pub enum Status {
    /// Lock acquired successfully.
    Acquired(Guard),
    /// Acquired lock is cyclic (same process).
    Cyclic,
    /// Lock is held by a different process.
    LockedBy(Owner),
    /// Lock is held, but the owner could not be identified.
    Unknown { pid: Option<Pid>, reason: String },
}

/// Error returned while terminating a lock owner or waiting for its lock to release.
#[derive(Debug)]
pub enum TakeoverError {
    /// The previously observed owner exited before it could be signalled.
    OwnerDisappeared { previous: Box<Owner> },
    /// The lock owner changed while takeover was in progress.
    OwnerChanged {
        previous: Box<Owner>,
        current: Box<Owner>,
    },
    /// Lock status became cyclic while waiting.
    Cyclic,
    /// The lock remained held by an unidentifiable owner.
    Unknown { pid: Option<Pid>, reason: String },
    /// Sending SIGTERM to the owner failed.
    SignalFailed { owner: Box<Owner> },
    /// SIGTERM is unavailable on this platform.
    SignalUnsupported { owner: Box<Owner> },
    /// The timeout elapsed before the lock could be acquired.
    Timeout {
        previous: Box<Owner>,
        timeout: Duration,
        last_observation: Box<Observation>,
    },
    /// An underlying tsdl error occurred while taking over the lock.
    Source(Error),
}

// ============================================================
// Structs
// ============================================================

/// A guard that holds an exclusive OS lock on the build directory lock file.
///
/// The lock is released when this guard is dropped. The lock file itself is
/// intentionally left on disk because OS locks are tied to open file handles,
/// not to path existence.
#[derive(Debug)]
pub struct Guard {
    file: File,
    lock_path: PathBuf,
}

/// Manages lock configuration and acquisition.
pub struct Lock {
    current_pid: Pid,
    lock_path: PathBuf,
}

/// Information about the process currently holding the build lock.
#[derive(Debug, Clone)]
pub struct Owner {
    pub pid: Pid,
    pub name: String,
    pub command: Option<String>,
    pub exe: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub status: String,
    pub run_time: u64,
    pub start_time: u64,
}

// ============================================================
// Free functions
// ============================================================

/// Join OS command arguments into a single display string.
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

/// Check if an IO error is caused by lock contention (`WouldBlock`).
fn is_lock_contention(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}

/// Check if two owner records refer to the same process instance (same PID and start time).
fn same_owner(current: &Owner, previous: &Owner) -> bool {
    current.pid == previous.pid && current.start_time == previous.start_time
}

// ============================================================
// Impls
// ============================================================

impl fmt::Display for Observation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockedBy(owner) => {
                write!(f, "lock is held by PID {} ({})", owner.pid, owner.name)
            }
            Self::Unknown { pid, reason } => {
                if let Some(pid) = pid {
                    write!(
                        f,
                        "lock is held by PID {pid}, but owner is unknown: {reason}"
                    )
                } else {
                    write!(f, "lock is held by an unknown owner: {reason}")
                }
            }
        }
    }
}

impl fmt::Display for Owner {
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

impl fmt::Display for TakeoverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OwnerDisappeared { previous } => write!(
                f,
                "Lock owner PID {} ({}) is no longer running; retry lock acquisition",
                previous.pid, previous.name
            ),
            Self::OwnerChanged { previous, current } => write!(
                f,
                "Lock owner changed while taking over: previous PID {} ({}), current PID {} ({})",
                previous.pid, previous.name, current.pid, current.name
            ),
            Self::Cyclic => write!(f, "Lock became cyclic while waiting for release"),
            Self::Unknown { pid, reason } => {
                if let Some(pid) = pid {
                    write!(f, "Could not identify build lock owner PID {pid}: {reason}")
                } else {
                    write!(f, "Could not identify build lock owner: {reason}")
                }
            }
            Self::SignalFailed { owner } => {
                write!(
                    f,
                    "Failed to send SIGTERM to lock owner PID {} ({})",
                    owner.pid, owner.name
                )
            }
            Self::SignalUnsupported { .. } => {
                write!(
                    f,
                    "SIGTERM is not supported on this platform; cannot terminate lock owner"
                )
            }
            Self::Timeout {
                previous,
                timeout,
                last_observation,
            } => write!(
                f,
                "Timed out after {} waiting for PID {} ({}) to release the build lock; last observation: {last_observation}",
                format_duration(*timeout),
                previous.pid,
                previous.name
            ),
            Self::Source(err) => write!(f, "{err}"),
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl From<Error> for TakeoverError {
    fn from(err: Error) -> Self {
        Self::Source(err)
    }
}

impl From<TakeoverError> for Error {
    fn from(err: TakeoverError) -> Self {
        Error::Message {
            message: err.to_string(),
        }
    }
}

impl Guard {
    /// Delete every entry in the build directory except the lock file itself
    /// and explicitly protected root-level files, such as the active log file.
    ///
    /// The build directory itself is preserved; only its children are removed.
    /// This is used by `--fresh` to clean the build directory without
    /// invalidating OS locks or unlinking files held open by the current process.
    pub fn clear_directory(&self, protected_files: &[PathBuf]) -> Result<()> {
        let build_dir = self.lock_path.parent().ok_or_else(|| Error::Message {
            message: format!(
                "Lock path has no parent directory: {}",
                self.lock_path.display()
            ),
        })?;
        let build_dir_abs = absolute_normalize(build_dir)?;
        let mut protected_names = HashSet::<OsString>::new();

        let lock_name = self
            .lock_path
            .file_name()
            .map_or_else(|| OsString::from(consts::LOCK_FILE), OsString::from);
        protected_names.insert(lock_name);

        for protected in protected_files {
            let protected_abs = absolute_normalize(protected)?;
            if protected_abs.parent() == Some(build_dir_abs.as_path())
                && let Some(name) = protected_abs.file_name()
            {
                protected_names.insert(name.to_os_string());
            }
        }

        for entry in fs::read_dir(build_dir)
            .with_context(|| format!("Reading build directory {}", build_dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("Reading directory entry in {}", build_dir.display()))?;

            let name = entry.file_name();

            if protected_names.contains(&name) {
                continue;
            }

            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("Statting {}", path.display()))?;

            if file_type.is_dir() {
                fs::remove_dir_all(&path)
                    .with_context(|| format!("Removing directory {}", path.display()))?;
            } else {
                fs::remove_file(&path)
                    .with_context(|| format!("Removing file {}", path.display()))?;
            }
        }

        info!("Cleaned {}", build_dir.display());
        Ok(())
    }
}

impl Lock {
    /// Create a lock manager for the given build directory.
    #[must_use]
    pub fn new(build_dir: &BuildDir) -> Self {
        Self {
            lock_path: build_dir.lock_file(),
            current_pid: Pid::from(process::id() as usize),
        }
    }

    /// Check lock status and acquire the OS lock if available.
    pub fn try_acquire(&self) -> Result<Status> {
        let file = self.open_lock_file()?;

        match file.try_lock_exclusive() {
            Ok(()) => self.activate(file).map(Status::Acquired),
            Err(err) if is_lock_contention(&err) => Ok(self.lock_status()),
            Err(err) => Err(Error::Context {
                message: format!("Acquiring build lock {}", self.lock_path.display()),
                source: err.into(),
            }),
        }
    }

    /// Send SIGTERM to the process that held the lock when `owner` was captured.
    pub fn terminate_owner(&self, owner: &Owner) -> StdResult<(), TakeoverError> {
        info!(
            "Sending SIGTERM to lock owner PID {} ({})",
            owner.pid, owner.name
        );
        let system = Self::system_for_pid(owner.pid);
        let process = system
            .process(owner.pid)
            .ok_or_else(|| TakeoverError::OwnerDisappeared {
                previous: Box::new(owner.clone()),
            })?;

        if process.start_time() != owner.start_time {
            let current =
                Self::owner_for_pid(owner.pid).ok_or_else(|| TakeoverError::OwnerDisappeared {
                    previous: Box::new(owner.clone()),
                })?;
            return Err(TakeoverError::OwnerChanged {
                previous: Box::new(owner.clone()),
                current: Box::new(current),
            });
        }

        match process.kill_with(Signal::Term) {
            Some(true) => {
                info!("Sent SIGTERM to lock owner PID {}", owner.pid);
                Ok(())
            }
            Some(false) => Err(TakeoverError::SignalFailed {
                owner: Box::new(owner.clone()),
            }),
            None => Err(TakeoverError::SignalUnsupported {
                owner: Box::new(owner.clone()),
            }),
        }
    }

    /// Wait for the build lock to become available after the lock owner was terminated.
    ///
    /// The OS lock remains the source of truth. Each poll tries to acquire it first;
    /// if it is still held, owner metadata is re-read so races with a new lock owner
    /// are reported explicitly instead of waiting on stale information.
    pub fn wait_for_release(
        &self,
        owner: &Owner,
        timeout: Duration,
    ) -> StdResult<Guard, TakeoverError> {
        info!(
            "Waiting up to {} for lock release from PID {}",
            crate::format_duration(timeout),
            owner.pid
        );

        let deadline = Instant::now() + timeout;
        let mut delay = Duration::from_millis(50);
        let mut last_observation = Observation::LockedBy(Box::new(owner.clone()));

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(TakeoverError::Timeout {
                    previous: Box::new(owner.clone()),
                    timeout,
                    last_observation: Box::new(last_observation),
                });
            }

            match self.try_acquire()? {
                Status::Acquired(guard) => return Ok(guard),

                Status::Cyclic => return Err(TakeoverError::Cyclic),

                Status::LockedBy(current) => {
                    if same_owner(&current, owner) {
                        last_observation = Observation::LockedBy(Box::new(current));
                    } else {
                        return Err(TakeoverError::OwnerChanged {
                            previous: Box::new(owner.clone()),
                            current: Box::new(current),
                        });
                    }
                }

                Status::Unknown { pid, reason } => {
                    // If metadata now points at a different PID that we cannot inspect,
                    // stop waiting on the previous owner. Otherwise keep polling until
                    // timeout so transient metadata reads or inherited lock handles have
                    // time to settle.
                    if pid.is_some_and(|pid| pid != owner.pid) {
                        return Err(TakeoverError::Unknown { pid, reason });
                    }
                    last_observation = Observation::Unknown { pid, reason };
                }
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            thread::sleep(delay.min(remaining));
            delay = delay.saturating_mul(2).min(Duration::from_millis(500));
        }
    }
    /// Write PID metadata and construct a Guard after acquiring the OS lock.
    fn activate(&self, mut file: File) -> Result<Guard> {
        self.write_metadata(&mut file)?;
        info!("Acquired lock on build directory");
        Ok(Guard {
            file,
            lock_path: self.lock_path.clone(),
        })
    }

    /// Open (or create) the lock file for exclusive access.
    fn open_lock_file(&self) -> Result<File> {
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Creating build directory {}", parent.display()))?;
        }

        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock_path)
            .with_context(|| format!("Opening lock file {}", self.lock_path.display()))
    }

    /// Helper for checking process status and determining lock conflicts.
    fn lock_status(&self) -> Status {
        let lock_pid = match self.read_pid() {
            Ok(pid) => pid,
            Err(err) => {
                return Status::Unknown {
                    pid: None,
                    reason: format!("lock is held, but owner metadata could not be read: {err}"),
                };
            }
        };

        if lock_pid == self.current_pid {
            return Status::Cyclic;
        }

        match Self::owner_for_pid(lock_pid) {
            Some(owner) => Status::LockedBy(owner),
            None => Status::Unknown {
                pid: Some(lock_pid),
                reason: "lock is held, but the metadata PID is not running or cannot be inspected"
                    .to_string(),
            },
        }
    }

    /// Read process metadata for a given PID (name, command, cwd, etc.).
    fn owner_for_pid(pid: Pid) -> Option<Owner> {
        let system = Self::system_for_pid(pid);
        let process = system.process(pid)?;

        Some(Owner {
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

    /// Read the PID value stored in the lock file.
    fn read_pid(&self) -> Result<Pid> {
        let content = fs::read_to_string(&self.lock_path)
            .with_context(|| format!("Reading lock file {}", self.lock_path.display()))?;

        let pid: usize = content.trim().parse().map_err(|_| Error::Message {
            message: format!(
                "Invalid PID '{}' in lock file {}",
                content.trim(),
                self.lock_path.display()
            ),
        })?;

        Ok(Pid::from(pid))
    }

    /// Create a System snapshot with process info for a single PID.
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

    /// Write the current PID into the lock file.
    fn write_metadata(&self, file: &mut File) -> Result<()> {
        file.set_len(0)
            .with_context(|| format!("Truncating lock file {}", self.lock_path.display()))?;
        file.seek(SeekFrom::Start(0))
            .with_context(|| format!("Seeking lock file {}", self.lock_path.display()))?;
        write!(file, "{}", self.current_pid.as_u32()).with_context(|| {
            format!(
                "Writing lock file {} with PID {}",
                self.lock_path.display(),
                self.current_pid
            )
        })?;
        file.sync_all()
            .with_context(|| format!("Syncing lock file {}", self.lock_path.display()))
    }
}

impl std::error::Error for TakeoverError {}

impl TakeoverError {
    /// Whether the caller should re-check lock status and retry the outer takeover loop.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::OwnerDisappeared { .. } | Self::OwnerChanged { .. }
        )
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn unused_pid() -> usize {
        let system = System::new_all();
        (1_000_000..10_000_000)
            .find(|pid| system.process(Pid::from(*pid)).is_none())
            .expect("could not find an unused PID for lock test")
    }

    #[test]
    fn stale_lock_metadata_does_not_prevent_acquiring_free_os_lock() {
        let temp = tempfile::tempdir().unwrap();
        let lock_file = temp.path().join(consts::LOCK_FILE);
        fs::write(&lock_file, unused_pid().to_string()).unwrap();

        let build_dir = BuildDir::new(temp.path()).unwrap();
        let lock = Lock::new(&build_dir);
        let status = lock.try_acquire().unwrap();

        assert!(
            matches!(status, Status::Acquired(_)),
            "stale lock metadata should not block acquiring a free OS lock"
        );
    }
}
