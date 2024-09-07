use std::{
    borrow::Cow,
    fmt::Display,
    sync::{Arc, Mutex},
    time,
};

use clap_verbosity_flag::{InfoLevel, Verbosity};
use console::style;
use enum_dispatch::enum_dispatch;
use log::Level;
use miette::{Context, IntoDiagnostic, Result};

use crate::{args::ProgressStyle, format_duration};

/// TODO: Get rid of the stupid progress bar crate.
///
/// The API is not nice, and I can't change the number of steps on the fly.
/// Which I need for repos declaring multiple parsers like php. I can't
/// change what's in the tick position easily too. And let's not mention
/// code duplication …
///
/// What Ineed is a single class that handles plain and fancy progress strategies,
/// instead of having to handle them with static dispatch via `enum_dispatch`.
///
/// PS: What' _"bad"_ about working with `enum_dispatch` is the language server.
/// Any modification to the trait you're dispatching will not properly propagate
/// and your diagnostics will be behind reality.

pub const TICK_CHARS: &str = "⠷⠯⠟⠻⠽⠾⠿";

#[must_use]
pub fn current(progress: &ProgressStyle, verbose: &Verbosity<InfoLevel>) -> Progress {
    verbose.log_level().map_or_else(
        || current_style(progress),
        |level| match level {
            Level::Debug | Level::Trace => Progress::Plain(Plain::default()),
            _ => current_style(progress),
        },
    )
}

fn current_style(progress: &ProgressStyle) -> Progress {
    if match progress {
        ProgressStyle::Auto => atty::is(atty::Stream::Stdout),
        ProgressStyle::Fancy => true,
        ProgressStyle::Plain => false,
    } {
        Progress::Fancy(Fancy::default())
    } else {
        Progress::Plain(Plain::default())
    }
}

#[derive(Debug, Clone)]
#[enum_dispatch(ProgressState)]
pub enum Progress {
    Plain(Plain),
    Fancy(Fancy),
}

#[derive(Debug, Clone, Default)]
pub struct Plain {
    handles: Vec<PlainHandle>,
}

#[derive(Debug, Clone, Default)]
pub struct Fancy {
    handles: Vec<FancyHandle>,
    multi: indicatif::MultiProgress,
}

#[enum_dispatch]
pub trait ProgressState {
    fn clear(&self) -> Result<()>;
    fn register(&mut self, name: impl Into<String>, num_tasks: usize) -> ProgressHandle;
    fn tick(&self);
    fn is_done(&self) -> bool;
}

#[derive(Debug, Clone)]
#[enum_dispatch(Handle)]
pub enum ProgressHandle {
    Plain(PlainHandle),
    Fancy(FancyHandle),
}

#[derive(Debug, Clone)]
pub struct PlainHandle {
    cur_task: Arc<Mutex<usize>>,
    name: Arc<String>,
    num_tasks: usize,
    t_start: Option<time::Instant>,
}

#[derive(Debug, Clone)]
pub struct FancyHandle {
    bar: indicatif::ProgressBar,
    name: Arc<String>,
    num_tasks: usize,
    t_start: Option<time::Instant>,
}

pub trait HandleMessage: Into<Cow<'static, str>> + Display {}
impl<T> HandleMessage for T where T: Into<Cow<'static, str>> + Display {}

#[enum_dispatch]
pub trait Handle {
    /// Declares end of execution with an error.
    fn err(&self, msg: impl HandleMessage);
    /// Declares end of execution with an success.
    fn fin(&self, msg: impl HandleMessage);
    /// Changes the displayed message for the current step.
    fn msg(&self, msg: impl HandleMessage);
    /// Declares transition to next step.
    fn step(&self, msg: impl HandleMessage);
    /// Through err or fin.
    fn is_done(&self) -> bool;
    /// Declares transition to first strp.
    fn start(&mut self, msg: impl HandleMessage);
    /// Useful for `Fancy` to redraw time and ticker.
    fn tick(&self);
}

// Implementations.

impl Fancy {
    #[must_use]
    pub fn new() -> Self {
        Fancy::default()
    }
}

impl Drop for Fancy {
    fn drop(&mut self) {
        for handle in &self.handles {
            handle.bar.finish();
        }
    }
}

impl ProgressState for Fancy {
    fn clear(&self) -> Result<()> {
        self.multi
            .clear()
            .into_diagnostic()
            .wrap_err("Clearing the multi-progress bar")
    }

    fn register(&mut self, name: impl Into<String>, num_tasks: usize) -> ProgressHandle {
        let style =
            indicatif::ProgressStyle::with_template("{prefix:.bold.dim} {spinner} {wide_msg}")
                .unwrap()
                .tick_chars(TICK_CHARS);
        let bar = self
            .multi
            .add(indicatif::ProgressBar::new(num_tasks as u64));
        bar.set_prefix(format!("[?/{num_tasks}]"));
        bar.set_style(style);
        let handle = FancyHandle {
            name: Arc::new(name.into()),
            bar,
            num_tasks,
            t_start: None,
        };
        self.handles.push(handle.clone());
        ProgressHandle::Fancy(handle)
    }

    fn tick(&self) {
        for bar in &self.handles {
            bar.tick();
        }
    }

    fn is_done(&self) -> bool {
        self.handles.iter().all(Handle::is_done)
    }
}

impl ProgressState for Plain {
    fn clear(&self) -> Result<()> {
        Ok(())
    }

    fn register(&mut self, name: impl Into<String>, num_tasks: usize) -> ProgressHandle {
        let handle = PlainHandle {
            cur_task: Arc::new(Mutex::new(0)),
            name: Arc::new(name.into()),
            num_tasks,
            t_start: None,
        };
        self.handles.push(handle.clone());
        ProgressHandle::Plain(handle)
    }

    fn tick(&self) {}

    fn is_done(&self) -> bool {
        self.handles.iter().all(Handle::is_done)
    }
}

impl FancyHandle {
    fn format_elapsed(&self) -> String {
        self.t_start
            .map(|start| {
                format!(
                    " in {}",
                    style(format_duration(time::Instant::now().duration_since(start))).yellow()
                )
            })
            .unwrap_or_default()
    }
}

impl Handle for FancyHandle {
    fn err(&self, msg: impl HandleMessage) {
        self.bar.abandon_with_message(format!(
            "{} {} {}{}",
            *self.name,
            style(msg.into()).blue(),
            style("failed").red(),
            self.format_elapsed()
        ));
    }

    fn fin(&self, msg: impl HandleMessage) {
        self.bar.inc(1);
        self.bar
            .set_prefix(format!("[{}/{}]", self.bar.position(), self.num_tasks));
        self.bar.finish_with_message(format!(
            "{} {} {}{}",
            *self.name,
            style(msg).blue(),
            style("done").green(),
            self.format_elapsed()
        ));
    }

    fn msg(&self, msg: impl HandleMessage) {
        self.bar
            .set_prefix(format!("[{}/{}]", self.bar.position(), self.num_tasks));
        self.bar.set_message(format!("{} {}", *self.name, msg));
    }

    fn step(&self, msg: impl HandleMessage) {
        self.bar.inc(1);
        self.bar
            .set_prefix(format!("[{}/{}]", self.bar.position(), self.num_tasks));
        self.bar.set_message(format!("{}: {}", *self.name, msg));
    }

    fn is_done(&self) -> bool {
        self.bar.is_finished()
    }

    fn start(&mut self, msg: impl HandleMessage) {
        self.t_start = Some(time::Instant::now());
        self.bar.inc(1);
        self.bar
            .set_prefix(format!("[{}/{}]", self.bar.position(), self.num_tasks));
        self.bar.set_message(format!("{} {}", *self.name, msg));
    }

    fn tick(&self) {
        self.bar.tick();
    }
}

impl PlainHandle {
    fn format_elapsed(&self) -> String {
        self.t_start
            .map(|start| {
                format!(
                    " in {}",
                    format_duration(time::Instant::now().duration_since(start))
                )
            })
            .unwrap_or_default()
    }
}

impl Handle for PlainHandle {
    fn err(&self, msg: impl HandleMessage) {
        eprintln!(
            "[{}/{}] {} {} {}{}",
            self.cur_task.lock().unwrap(),
            self.num_tasks,
            *self.name,
            style(msg.into()).blue(),
            style("failed").red(),
            self.format_elapsed()
        );
    }

    fn fin(&self, msg: impl HandleMessage) {
        let cur_task = {
            let mut res = self.cur_task.lock().unwrap();
            *res += 1;
            *res
        };
        eprintln!(
            "[{}/{}] {} {} {}{}",
            cur_task,
            self.num_tasks,
            *self.name,
            style(msg).blue(),
            style("done").green(),
            self.format_elapsed()
        );
    }

    fn msg(&self, msg: impl HandleMessage) {
        eprintln!(
            "[{}/{}] {}: {}",
            self.cur_task.lock().unwrap(),
            self.num_tasks,
            *self.name,
            msg
        );
    }

    fn step(&self, msg: impl HandleMessage) {
        let cur_task = {
            let mut res = self.cur_task.lock().unwrap();
            *res += 1;
            *res
        };
        eprintln!("[{}/{}] {} {}", cur_task, self.num_tasks, *self.name, msg);
    }

    fn is_done(&self) -> bool {
        *self.cur_task.lock().unwrap() != self.num_tasks
    }

    fn start(&mut self, msg: impl HandleMessage) {
        self.t_start = Some(time::Instant::now());
        let cur_task = {
            let mut res = self.cur_task.lock().unwrap();
            *res += 1;
            *res
        };
        eprintln!("[{}/{}] {} {}", cur_task, self.num_tasks, *self.name, msg);
    }

    fn tick(&self) {}
}
