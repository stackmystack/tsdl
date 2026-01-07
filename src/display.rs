use std::{
    sync::atomic::Ordering,
    sync::{atomic::AtomicU64, Arc},
    time,
};

use clap_verbosity_flag::{InfoLevel, Verbosity};
use console::style;
use log::Level;
use tokio::sync::OnceCell;

use crate::{args::ProgressStyle, error::TsdlError, format_duration, git::GitRef, TsdlResult};

#[derive(Debug, Clone, Copy)]
pub enum UpdateKind {
    Msg,
    Step,
    Fin,
    Err,
}

/// Spinning sprite.
pub const TICK_CHARS: &str = "⠷⠯⠟⠻⠽⠾⠿";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Fancy,
    Plain,
}

#[derive(Debug, Clone)]
pub struct Progress {
    multi: indicatif::MultiProgress,
    pub mode: Mode,
    // We store handles to ensure they aren't dropped prematurely if needed,
    // mimicking the original `handles` vectors.
    handles: Vec<ProgressBar>,
}

impl Progress {
    #[must_use] 
    pub fn new(mode: Mode) -> Self {
        Self {
            multi: indicatif::MultiProgress::new(),
            mode,
            handles: Vec::new(),
        }
    }

    pub fn clear(&self) -> TsdlResult<()> {
        if self.mode == Mode::Fancy {
            self.multi
                .clear()
                .map_err(|e| TsdlError::context("Clearing the multi-progress bar", e))?;
        }
        Ok(())
    }

    pub fn is_done(&self) -> bool {
        self.handles.iter().all(ProgressBar::is_done)
    }

    pub fn prinltn(&self, msg: impl AsRef<str>) {
        println!("{}", msg.as_ref());
    }

    /// # Panics
    ///
    /// Will panic indicatif errs.
    pub fn register(&mut self, name: Arc<str>, git_ref: GitRef, num_tasks: usize) -> ProgressBar {
        let bar = match self.mode {
            Mode::Fancy => {
                let bar = indicatif::ProgressBar::new(num_tasks as u64);
                let bar = self.multi.add(bar);
                let style = indicatif::ProgressStyle::with_template(
                    "{prefix:.bold.dim} {spinner} {wide_msg}",
                )
                .unwrap_or_else(|_| {
                    panic!("cannot create spinner [?/{num_tasks}] {name} @ {git_ref}")
                })
                .tick_chars(TICK_CHARS);
                bar.set_style(style);
                bar.set_prefix(format!("[?/{num_tasks}]"));
                Some(bar)
            }
            Mode::Plain => None,
        };

        let handle = ProgressBar {
            bar,
            name,
            git_ref,
            num_tasks,
            t_start: OnceCell::new(),
            mode: self.mode,
            current_step: Arc::new(AtomicU64::new(0)),
        };

        self.handles.push(handle.clone());
        handle
    }

    pub fn tick(&self) {
        // Only necessary for fancy bars in some terminals/configs, plain bars do nothing
        if self.mode == Mode::Fancy {
            for handle in &self.handles {
                handle.tick();
            }
        }
    }
}

// Ensure bars are finished on drop
impl Drop for Progress {
    fn drop(&mut self) {
        for handle in &self.handles {
            if !handle.is_done() {
                if let Some(bar) = &handle.bar {
                    bar.finish();
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProgressBar {
    bar: Option<indicatif::ProgressBar>,
    pub name: Arc<str>,
    git_ref: GitRef,
    num_tasks: usize,
    t_start: OnceCell<time::Instant>,
    mode: Mode,
    current_step: Arc<AtomicU64>,
}

impl PartialEq for ProgressBar {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.git_ref == other.git_ref
            && self.num_tasks == other.num_tasks
    }
}

impl ProgressBar {
    fn format_elapsed(&self) -> String {
        self.t_start
            .get()
            .map(|start| {
                let dur = format_duration(time::Instant::now().duration_since(*start));
                if self.mode == Mode::Fancy {
                    format!(" in {}", style(dur).yellow())
                } else {
                    format!(" in {dur}")
                }
            })
            .unwrap_or_default()
    }

    fn name_with_version(&self) -> String {
        if self.git_ref.is_empty() {
            self.name.to_string()
        } else {
            format!("{} {}", self.name, style(&self.git_ref).blue())
        }
    }

    /// Helper to print log lines in Plain mode (using bar.println to coordinate with `MultiProgress`)
    pub fn println(&self, msg: String) {
        match &self.bar {
            Some(bar) => bar.println(msg),
            None => println!("{msg}"),
        }
    }
}

impl ProgressBar {
    pub fn err(&self, msg: impl AsRef<str>) {
        if let Some(bar) = &self.bar {
            bar.abandon_with_message(format!(
                "{} {} {}{}",
                self.name_with_version(),
                style(msg.as_ref()).blue(),
                style("failed").red(),
                self.format_elapsed()
            ));
        } else {
            let cur = self.current_step.load(Ordering::SeqCst);
            self.println(format!(
                "[{}/{}] {} {} {}{}",
                cur,
                self.num_tasks,
                self.name_with_version(),
                msg.as_ref(),
                style("failed").red(),
                self.format_elapsed()
            ));
        }
    }

    pub fn fin(&self, msg: impl AsRef<str>) {
        if let Some(bar) = &self.bar {
            bar.inc(1);
        } else {
            self.current_step.fetch_add(1, Ordering::SeqCst);
        }

        if let Some(bar) = &self.bar {
            let position = usize::try_from(bar.position())
                .unwrap_or(self.num_tasks)
                .min(self.num_tasks);
            bar.set_prefix(format!("[{}/{}]", position, self.num_tasks));

            let message = if msg.as_ref().is_empty() {
                format!(
                    "{} {}{}",
                    self.name_with_version(),
                    style("done").green(),
                    self.format_elapsed()
                )
            } else {
                format!(
                    "{} {} {}{}",
                    self.name_with_version(),
                    msg.as_ref(),
                    style("done").green(),
                    self.format_elapsed()
                )
            };
            bar.finish_with_message(message);
        } else {
            let cur = self.current_step.load(Ordering::SeqCst);
            if msg.as_ref().is_empty() {
                self.println(format!(
                    "[{}/{}] {} {}{}",
                    cur,
                    self.num_tasks,
                    self.name_with_version(),
                    style("done").green(),
                    self.format_elapsed()
                ));
            } else {
                self.println(format!(
                    "[{}/{}] {} {} {}{}",
                    cur,
                    self.num_tasks,
                    self.name_with_version(),
                    style(msg.as_ref()).blue(),
                    style("done").green(),
                    self.format_elapsed()
                ));
            }
        }
    }

    pub fn is_done(&self) -> bool {
        self.bar
            .as_ref()
            .is_some_and(indicatif::ProgressBar::is_finished)
    }

    pub fn msg(&self, msg: impl AsRef<str>) {
        if let Some(bar) = &self.bar {
            let position = usize::try_from(bar.position())
                .unwrap_or(self.num_tasks)
                .min(self.num_tasks);
            bar.set_prefix(format!("[{}/{}]", position, self.num_tasks));
            bar.set_message(format!("{} {}", self.name_with_version(), msg.as_ref()));
        } else {
            let cur = self.current_step.load(Ordering::SeqCst);
            self.println(format!(
                "[{}/{}] {}: {}",
                cur,
                self.num_tasks,
                self.name_with_version(),
                msg.as_ref()
            ));
        }
    }

    pub fn step(&self, msg: impl AsRef<str>) {
        let _ = self.t_start.set(time::Instant::now());
        if let Some(bar) = &self.bar {
            bar.inc(1);
        } else {
            self.current_step.fetch_add(1, Ordering::SeqCst);
        }

        if let Some(bar) = &self.bar {
            let position = usize::try_from(bar.position())
                .unwrap_or(self.num_tasks)
                .min(self.num_tasks);
            bar.set_prefix(format!("[{}/{}]", position, self.num_tasks));
            bar.set_message(format!("{}: {}", self.name_with_version(), msg.as_ref()));
        } else {
            let cur = self.current_step.load(Ordering::SeqCst);
            self.println(format!(
                "[{}/{}] {} {}",
                cur,
                self.num_tasks,
                self.name_with_version(),
                msg.as_ref()
            ));
        }
    }

    pub fn tick(&self) {
        if let Some(bar) = &self.bar {
            bar.tick();
        }
    }
}

#[must_use]
pub fn current(progress: &ProgressStyle, verbose: &Verbosity<InfoLevel>) -> Progress {
    let mut mode = match progress {
        ProgressStyle::Auto => {
            if atty::is(atty::Stream::Stdout) {
                Mode::Fancy
            } else {
                Mode::Plain
            }
        }
        ProgressStyle::Fancy => Mode::Fancy,
        ProgressStyle::Plain => Mode::Plain,
    };

    if matches!(verbose.log_level(), Some(Level::Debug | Level::Trace)) {
        mode = Mode::Plain;
    }

    Progress::new(mode)
}
