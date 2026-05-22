use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
// Item lifecycle + outcome
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemStatus {
    /// Just registered, not yet started.
    New,
    /// In progress — cloning, generating, building, etc.
    InProgress,
    /// Successfully completed.
    Done,
    /// Failed.
    Failed,
}

impl ItemStatus {
    fn icon(self) -> &'static str {
        match self {
            ItemStatus::New | ItemStatus::InProgress => "●",
            ItemStatus::Done => "✓",
            ItemStatus::Failed => "✗",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOutcome {
    /// We do not yet know whether this item is cached or needs work.
    Unknown,
    /// Cache hit.
    Cached,
    /// Not cached; work was needed.
    Built,
}

impl ItemOutcome {
    fn color(self) -> Color {
        match self {
            ItemOutcome::Unknown => Color::DarkGray,
            ItemOutcome::Cached => Color::Yellow,
            ItemOutcome::Built => Color::Blue,
        }
    }
}

fn name_color(status: ItemStatus, outcome: ItemOutcome) -> Color {
    match status {
        ItemStatus::Failed => Color::Red,
        ItemStatus::New | ItemStatus::InProgress | ItemStatus::Done => outcome.color(),
    }
}

fn indicator_color(status: ItemStatus) -> Color {
    match status {
        ItemStatus::Done => Color::Green,
        ItemStatus::Failed => Color::Red,
        ItemStatus::New | ItemStatus::InProgress => Color::DarkGray,
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
    pub outcome: ItemOutcome,
    pub msg: Arc<str>,
    pub step: usize,
    pub total: usize,
    /// Creation time for the whole item. This is not reset between steps.
    pub started_at: Instant,
    /// Frozen whole-item elapsed time for terminal states.
    pub frozen_elapsed: Option<Duration>,
}

#[derive(Debug, Clone)]
pub(crate) struct GrammarEntry {
    pub repo: Arc<str>,
    pub repo_id: u64,
    pub name: Arc<str>,
    pub git_ref: GitRef,
    pub status: ItemStatus,
    pub outcome: ItemOutcome,
    pub msg: Arc<str>,
    pub step: usize,
    pub total: usize,
    /// Creation time for the whole item. This is not reset between steps.
    pub started_at: Instant,
    /// Frozen whole-item elapsed time for terminal states.
    pub frozen_elapsed: Option<Duration>,
}

// ---------------------------------------------------------------------------
// Render rows
// ---------------------------------------------------------------------------

struct RenderRow {
    status: ItemStatus,
    outcome: ItemOutcome,
    name: String,
    git_ref: String,
    msg: String,
    step: usize,
    total: usize,
    started_at: Instant,
    frozen_elapsed: Option<Duration>,
    indent: &'static str,
}

impl RenderRow {
    fn elapsed(&self) -> Duration {
        self.frozen_elapsed
            .unwrap_or_else(|| self.started_at.elapsed())
    }

    fn time(&self) -> String {
        format_elapsed_duration(self.elapsed())
    }

    fn step_text(&self) -> String {
        if self.total > 0 {
            format!("[{}/{}]", self.step.min(self.total), self.total)
        } else {
            String::new()
        }
    }

    fn display_name(&self) -> String {
        format!("{}{}", self.indent, self.name)
    }
}

struct RenderLayout {
    time_width: usize,
    ref_width: usize,
    step_width: usize,
    name_width: usize,
}

impl RenderLayout {
    fn from_rows(rows: &[RenderRow]) -> Self {
        Self {
            time_width: rows
                .iter()
                .map(|row| row.time().chars().count())
                .max()
                .unwrap_or(5)
                .max(5),
            ref_width: rows
                .iter()
                .map(|row| row.git_ref.chars().count())
                .max()
                .unwrap_or(4)
                .max(4),
            step_width: rows
                .iter()
                .map(|row| row.step_text().chars().count())
                .max()
                .unwrap_or(5)
                .max(5),
            name_width: rows
                .iter()
                .map(|row| row.display_name().chars().count())
                .max()
                .unwrap_or(4)
                .max(4),
        }
    }
}

// ---------------------------------------------------------------------------
// DisplayState — the full renderable state
// ---------------------------------------------------------------------------

pub(crate) struct DisplayState {
    pub mode: Mode,
    pub repos: HashMap<u64, RepoEntry>,
    pub grammars: HashMap<u64, GrammarEntry>,
    pub build_dir: Arc<PathBuf>,
    pub out_dir: Arc<PathBuf>,
}

impl DisplayState {
    pub fn new(mode: Mode, build_dir: Arc<PathBuf>, out_dir: Arc<PathBuf>) -> Self {
        Self {
            mode,
            repos: HashMap::new(),
            grammars: HashMap::new(),
            build_dir,
            out_dir,
        }
    }

    /// Render the entire display as ratatui `Line`s.
    pub fn render_lines(&self, width: u16) -> Vec<Line<'_>> {
        let w = width as usize;
        let mut lines: Vec<Line> = Vec::new();
        let rows = self.render_rows();
        let layout = RenderLayout::from_rows(&rows);

        for row in &rows {
            lines.push(self.format_item_line(row, &layout, w));
        }

        if !lines.is_empty() {
            lines.push(Line::from(""));
        }

        lines.push(self.format_footer_build());
        lines.push(self.format_footer_out());
        lines.push(Line::from(""));
        lines.push(self.format_footer_counts());

        lines
    }

    fn sorted_repos(&self) -> Vec<(u64, &RepoEntry)> {
        let mut repos: Vec<(u64, &RepoEntry)> = self
            .repos
            .iter()
            .map(|(repo_id, repo)| (*repo_id, repo))
            .collect();
        repos.sort_by_key(|(_, r)| r.started_at);
        repos
    }

    fn sorted_grammars_for_repo(&self, repo_id: u64) -> Vec<&GrammarEntry> {
        let mut grammars: Vec<&GrammarEntry> = self
            .grammars
            .values()
            .filter(|g| g.repo_id == repo_id)
            .collect();
        grammars.sort_by_key(|g| g.started_at);
        grammars
    }

    fn render_rows(&self) -> Vec<RenderRow> {
        let mut rows = Vec::new();

        for (repo_id, repo) in self.sorted_repos() {
            let repo_grammars = self.sorted_grammars_for_repo(repo_id);

            if repo_grammars.is_empty() {
                rows.push(RenderRow::from_repo(repo, ""));
            } else if repo_grammars.len() == 1 {
                let grammar = repo_grammars[0];
                if grammar.name == repo.name {
                    rows.push(RenderRow::from_grammar(grammar, repo.name.to_string(), ""));
                } else {
                    rows.push(RenderRow::from_grammar(
                        grammar,
                        format!("{}/{}", repo.name, grammar.name),
                        "",
                    ));
                }
            } else {
                rows.push(RenderRow::from_repo(repo, ""));
                for grammar in repo_grammars {
                    rows.push(RenderRow::from_grammar(
                        grammar,
                        grammar.name.to_string(),
                        "  ",
                    ));
                }
            }
        }

        rows
    }

    fn dim_line(&self, text: String) -> Line<'static> {
        Line::from(Span::styled(
            text,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ))
    }

    fn format_item_line(
        &self,
        row: &RenderRow,
        layout: &RenderLayout,
        width: usize,
    ) -> Line<'static> {
        let time = row.time();
        let step = row.step_text();
        let name = row.display_name();
        let icon = row.status.icon();

        let time_col = format!("{time:<width$}", width = layout.time_width);
        let ref_col = format!("{:<width$}", row.git_ref, width = layout.ref_width);
        let step_col = format!("{:>width$}", step, width = layout.step_width);
        let name_col = format!("{:<width$}", name, width = layout.name_width);

        let fixed_width = layout.time_width
            + 1
            + layout.ref_width
            + 1
            + layout.step_width
            + 1
            + icon.chars().count()
            + 1
            + layout.name_width
            + 1;
        let msg_width = width.saturating_sub(fixed_width);
        let msg_col = truncate_str(&row.msg, msg_width.max(1));

        let progress_style = Style::default().fg(Color::White);
        let icon_style = Style::default()
            .fg(indicator_color(row.status))
            .add_modifier(Modifier::BOLD);
        let name_style = Style::default()
            .fg(name_color(row.status, row.outcome))
            .add_modifier(Modifier::BOLD);
        let msg_style = Style::default().fg(Color::White);

        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(time_col, progress_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(ref_col, progress_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(step_col, progress_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(icon, icon_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(name_col, name_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(msg_col, msg_style));

        Line::from(spans)
    }

    fn format_footer_build(&self) -> Line<'static> {
        self.dim_line(format!("build: {}", self.build_dir.display()))
    }

    fn format_footer_out(&self) -> Line<'static> {
        self.dim_line(format!("out:   {}", self.out_dir.display()))
    }

    fn format_footer_counts(&self) -> Line<'static> {
        let (cached, built, building, failed) = self.summary_counts();
        let mut spans: Vec<Span> = Vec::new();

        push_summary_success(&mut spans, cached, "cached", Color::Yellow, true);
        push_summary_success(&mut spans, built, "built", Color::Blue, false);
        if building > 0 {
            push_summary_building(&mut spans, building);
        }
        push_summary_failed(&mut spans, failed);

        Line::from(spans)
    }

    pub(crate) fn summary_counts(&self) -> (usize, usize, usize, usize) {
        let mut cached = 0;
        let mut built = 0;
        let mut building = 0;
        let mut failed = 0;

        for grammar in self.grammars.values() {
            count_grammar_status(
                grammar.status,
                grammar.outcome,
                &mut cached,
                &mut built,
                &mut building,
                &mut failed,
            );
        }

        for (repo_id, repo) in self.sorted_repos() {
            let has_grammars = self.grammars.values().any(|g| g.repo_id == repo_id);
            if !has_grammars && repo.status == ItemStatus::Failed {
                failed += 1;
            }
        }

        (cached, built, building, failed)
    }
}

impl RenderRow {
    fn from_repo(repo: &RepoEntry, indent: &'static str) -> Self {
        Self {
            status: repo.status,
            outcome: repo.outcome,
            name: repo.name.to_string(),
            git_ref: repo.git_ref.to_string(),
            msg: repo.msg.to_string(),
            step: repo.step,
            total: repo.total,
            started_at: repo.started_at,
            frozen_elapsed: repo.frozen_elapsed,
            indent,
        }
    }

    fn from_grammar(grammar: &GrammarEntry, name: String, indent: &'static str) -> Self {
        Self {
            status: grammar.status,
            outcome: grammar.outcome,
            name,
            git_ref: grammar.git_ref.to_string(),
            msg: grammar.msg.to_string(),
            step: grammar.step,
            total: grammar.total,
            started_at: grammar.started_at,
            frozen_elapsed: grammar.frozen_elapsed,
            indent,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn count_grammar_status(
    status: ItemStatus,
    outcome: ItemOutcome,
    cached: &mut usize,
    built: &mut usize,
    building: &mut usize,
    failed: &mut usize,
) {
    match status {
        ItemStatus::New | ItemStatus::InProgress => *building += 1,
        ItemStatus::Done => match outcome {
            ItemOutcome::Cached => *cached += 1,
            ItemOutcome::Built | ItemOutcome::Unknown => *built += 1,
        },
        ItemStatus::Failed => *failed += 1,
    }
}

fn push_summary_success(
    spans: &mut Vec<Span<'static>>,
    count: usize,
    label: &'static str,
    label_color: Color,
    leading: bool,
) {
    if !leading {
        spans.push(Span::raw("  "));
    }

    if count > 0 {
        spans.push(Span::styled(
            format!("✓ {count} "),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(label_color)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled(
            format!("✓ {count} {label}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
}

fn push_summary_building(spans: &mut Vec<Span<'static>>, count: usize) {
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("{count} building"),
        Style::default().fg(if count > 0 {
            Color::White
        } else {
            Color::DarkGray
        }),
    ));
}

fn push_summary_failed(spans: &mut Vec<Span<'static>>, count: usize) {
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("✗ {count} failed"),
        Style::default().fg(if count > 0 {
            Color::Red
        } else {
            Color::DarkGray
        }),
    ));
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max <= 1 {
        "…".to_string()
    } else {
        let mut truncated: String = s.chars().take(max - 1).collect();
        truncated.push('…');
        truncated
    }
}

fn format_elapsed_duration(dur: Duration) -> String {
    let secs = dur.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.2}s")
    } else {
        format!("{:.2}m", secs / 60.0)
    }
}
