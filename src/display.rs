use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap_verbosity_flag::{InfoLevel, Verbosity};
use log::Level;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::args::ProgressStyle;
use crate::git::GitRef;

// ---------------------------------------------------------------------------
// Mode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Fancy,
    Plain,
}

/// Determine the display mode from CLI flags and environment.
#[must_use]
pub fn mode_from_args(progress: &ProgressStyle, verbose: &Verbosity<InfoLevel>) -> Mode {
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

    mode
}

// ---------------------------------------------------------------------------
// Item status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemStatus {
    /// Just registered, not yet started (blue).
    New,
    /// In progress — cloning, generating, building, etc. (yellow).
    InProgress,
    /// Cache hit, no rebuild needed (green).
    Cached,
    /// Successfully built (green).
    Done,
    /// Failed (red).
    Failed,
}

impl ItemStatus {
    fn color(self) -> Color {
        match self {
            ItemStatus::New => Color::Blue,
            ItemStatus::InProgress => Color::Yellow,
            ItemStatus::Cached => Color::Green,
            ItemStatus::Done => Color::Green,
            ItemStatus::Failed => Color::Red,
        }
    }

    fn icon(self) -> &'static str {
        match self {
            ItemStatus::New => "●",
            ItemStatus::InProgress => "●",
            ItemStatus::Cached => "✓",
            ItemStatus::Done => "✓",
            ItemStatus::Failed => "✗",
        }
    }
}

// ---------------------------------------------------------------------------
// State entries
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct RepoEntry {
    pub name: Arc<str>,
    pub git_ref: GitRef,
    pub status: ItemStatus,
    pub msg: Arc<str>,
    pub step: usize,
    pub total: usize,
    pub t_start: Instant,
    /// Frozen elapsed string for Done/Failed states.
    pub frozen_elapsed: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct GrammarEntry {
    pub repo: Arc<str>,
    pub repo_id: u64,
    pub name: Arc<str>,
    pub git_ref: GitRef,
    pub status: ItemStatus,
    pub msg: Arc<str>,
    pub step: usize,
    pub total: usize,
    pub t_start: Instant,
    /// Frozen elapsed string for Done/Cached/Failed states.
    pub frozen_elapsed: Option<String>,
}

// ---------------------------------------------------------------------------
// DisplayState — the full renderable state
// ---------------------------------------------------------------------------

pub(crate) struct DisplayState {
    pub mode: Mode,
    pub repos: HashMap<u64, RepoEntry>,
    pub grammars: HashMap<u64, GrammarEntry>,
    pub cached_count: usize,
    pub built_count: usize,
    pub failed_count: usize,
    pub build_dir: Arc<PathBuf>,
    pub out_dir: Arc<PathBuf>,
    pub total_start: Instant,
}

impl DisplayState {
    pub fn new(mode: Mode, build_dir: Arc<PathBuf>, out_dir: Arc<PathBuf>) -> Self {
        Self {
            mode,
            repos: HashMap::new(),
            grammars: HashMap::new(),
            cached_count: 0,
            built_count: 0,
            failed_count: 0,
            build_dir,
            out_dir,
            total_start: Instant::now(),
        }
    }

    /// Render the entire display as ratatui `Line`s.
    pub fn render_lines(&self, width: u16) -> Vec<Line<'_>> {
        let w = width as usize;
        let mut lines: Vec<Line> = Vec::new();

        // ── Legend (always at top) ──
        lines.push(self.format_legend(w));

        // ── Repos + grammars (persistent, never removed) ──
        let sorted_repos = self.sorted_repos();
        if !sorted_repos.is_empty() {
            lines.push(Line::from(""));

            for repo in &sorted_repos {
                let repo_lines = self.format_repo_with_grammars(repo, w);
                lines.extend(repo_lines);
            }
        }

        // ── Footer ──
        lines.push(self.separator(w, "─"));
        lines.push(self.format_footer_build(w));
        lines.push(self.format_footer_out(w));
        lines.push(Line::from(""));
        lines.push(self.format_footer_counts(w));

        lines
    }

    fn sorted_repos(&self) -> Vec<&RepoEntry> {
        let mut repos: Vec<&RepoEntry> = self.repos.values().collect();
        repos.sort_by_key(|r| r.t_start);
        repos
    }

    fn format_repo_with_grammars(&self, repo: &RepoEntry, w: usize) -> Vec<Line<'static>> {
        let mut lines: Vec<Line> = Vec::new();
        let repo_grammars: Vec<&GrammarEntry> = self
            .grammars
            .values()
            .filter(|g| g.repo_id == self.repo_id_by_name(&repo.name))
            .collect();

        if repo_grammars.is_empty() {
            lines.push(self.format_item_line(
                repo.status,
                &repo.name,
                &repo.git_ref.to_string(),
                &repo.msg,
                repo.step,
                repo.total,
                repo.t_start,
                &repo.frozen_elapsed,
                w,
                "",
            ));
        } else if repo_grammars.len() == 1 {
            let g = repo_grammars[0];
            if g.name == repo.name {
                lines.push(self.format_item_line(
                    g.status,
                    &repo.name,
                    &repo.git_ref.to_string(),
                    &g.msg,
                    g.step,
                    g.total,
                    g.t_start,
                    &g.frozen_elapsed,
                    w,
                    "",
                ));
            } else {
                let label = format!("{}/{}", repo.name, g.name);
                lines.push(self.format_item_line(
                    g.status,
                    &label,
                    &g.git_ref.to_string(),
                    &g.msg,
                    g.step,
                    g.total,
                    g.t_start,
                    &g.frozen_elapsed,
                    w,
                    "",
                ));
            }
        } else {
            lines.push(self.format_item_line(
                repo.status,
                &repo.name,
                &repo.git_ref.to_string(),
                &repo.msg,
                repo.step,
                repo.total,
                repo.t_start,
                &repo.frozen_elapsed,
                w,
                "",
            ));
            for g in &repo_grammars {
                lines.push(self.format_item_line(
                    g.status,
                    &g.name,
                    &g.git_ref.to_string(),
                    &g.msg,
                    g.step,
                    g.total,
                    g.t_start,
                    &g.frozen_elapsed,
                    w,
                    "  ",
                ));
            }
        }

        lines
    }

    fn repo_id_by_name(&self, name: &str) -> u64 {
        self.repos
            .iter()
            .find(|(_, r)| r.name.as_ref() == name)
            .map(|(id, _)| *id)
            .unwrap_or(0)
    }

    fn separator(&self, width: usize, ch: &str) -> Line<'static> {
        let sep = ch.repeat(width.min(120));
        Line::from(Span::styled(
            sep,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ))
    }

    fn dim_line(&self, text: String) -> Line<'static> {
        Line::from(Span::styled(
            text,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ))
    }

    fn format_legend(&self, _width: usize) -> Line<'static> {
        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(
            "  cached",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            "  done",
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            "  failed",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
        Line::from(spans)
    }

    fn format_item_line(
        &self,
        status: ItemStatus,
        name: &str,
        git_ref: &str,
        msg: &str,
        step: usize,
        total: usize,
        t_start: Instant,
        frozen_elapsed: &Option<String>,
        width: usize,
        indent: &str,
    ) -> Line<'static> {
        let icon = status.icon();
        let name_style = Style::default()
            .fg(status.color())
            .add_modifier(Modifier::BOLD);
        let msg_style = Style::default().fg(Color::White);

        let step_info = if total > 0 {
            format!("[{}/{}] ", step.min(total), total)
        } else {
            String::new()
        };

        // Elapsed: frozen for terminal states, live otherwise
        let elapsed = if let Some(ref frozen) = frozen_elapsed {
            format!("in {frozen}")
        } else if status == ItemStatus::InProgress || status == ItemStatus::New {
            format_elapsed(t_start)
        } else {
            String::new()
        };

        let line_content = format!("{step_info}{}", truncate_str(msg, width.saturating_sub(40)));

        let prefix = format!(
            "{indent} {} {:<28} {}",
            icon,
            truncate_str(name, 26),
            truncate_str(git_ref, 12)
        );

        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(prefix, name_style));
        spans.push(Span::styled(line_content, msg_style));
        if !elapsed.is_empty() {
            let elapsed_color = match status {
                ItemStatus::Done | ItemStatus::Cached => Color::Green,
                ItemStatus::Failed => Color::Red,
                _ => Color::DarkGray,
            };
            spans.push(Span::styled(
                format!("  {elapsed}"),
                Style::default()
                    .fg(elapsed_color)
                    .add_modifier(if frozen_elapsed.is_some() {
                        Modifier::empty()
                    } else {
                        Modifier::DIM
                    }),
            ));
        }

        Line::from(spans)
    }

    fn format_footer_build(&self, _width: usize) -> Line<'static> {
        self.dim_line(format!("  build: {}", self.build_dir.display()))
    }

    fn format_footer_out(&self, _width: usize) -> Line<'static> {
        self.dim_line(format!("  out:   {}", self.out_dir.display()))
    }

    fn format_footer_counts(&self, _width: usize) -> Line<'static> {
        let in_progress = self
            .repos
            .values()
            .filter(|r| r.status == ItemStatus::New || r.status == ItemStatus::InProgress)
            .count()
            + self
                .grammars
                .values()
                .filter(|g| g.status == ItemStatus::New || g.status == ItemStatus::InProgress)
                .count();

        let total_elapsed = format_elapsed(self.total_start);

        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(
            format!("  ✓ {} cached  ", self.cached_count),
            Style::default().fg(if self.cached_count > 0 {
                Color::Green
            } else {
                Color::DarkGray
            }),
        ));
        spans.push(Span::styled(
            format!("● {} done  ", self.built_count),
            Style::default().fg(if self.built_count > 0 {
                Color::Blue
            } else {
                Color::DarkGray
            }),
        ));
        spans.push(Span::styled(
            format!("● {} building  ", in_progress),
            Style::default().fg(if in_progress > 0 {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
        ));
        spans.push(Span::styled(
            format!("✗ {} failed  ", self.failed_count),
            Style::default().fg(if self.failed_count > 0 {
                Color::Red
            } else {
                Color::DarkGray
            }),
        ));
        spans.push(Span::styled(
            format!("Done in {total_elapsed}"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::DIM),
        ));

        Line::from(spans)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else if max <= 3 {
        &s[..max]
    } else {
        &s[..max - 3]
    }
}

fn format_elapsed(start: Instant) -> String {
    let dur = start.elapsed();
    let secs = dur.as_secs();
    if secs < 60 {
        format!("{}.{:02}s", secs, dur.subsec_millis() / 10)
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}
