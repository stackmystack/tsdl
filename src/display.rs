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

    fn is_terminal(self) -> bool {
        matches!(self, ItemStatus::Done | ItemStatus::Failed)
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

fn palette_color(status: ItemStatus, outcome: ItemOutcome) -> Color {
    match status {
        ItemStatus::Failed => Color::Red,
        ItemStatus::New | ItemStatus::InProgress | ItemStatus::Done => outcome.color(),
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

        for (repo_id, repo) in self.sorted_repos() {
            let repo_lines = self.format_repo_with_grammars(repo_id, repo, w);
            lines.extend(repo_lines);
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

    fn format_repo_with_grammars(
        &self,
        repo_id: u64,
        repo: &RepoEntry,
        w: usize,
    ) -> Vec<Line<'static>> {
        let mut lines: Vec<Line> = Vec::new();
        let repo_grammars = self.sorted_grammars_for_repo(repo_id);

        if repo_grammars.is_empty() {
            lines.push(self.format_item_line(
                repo.status,
                repo.outcome,
                &repo.name,
                &repo.git_ref.to_string(),
                &repo.msg,
                repo.step,
                repo.total,
                repo.started_at,
                repo.frozen_elapsed,
                w,
                "",
            ));
        } else if repo_grammars.len() == 1 {
            let g = repo_grammars[0];
            if g.name == repo.name {
                lines.push(self.format_item_line(
                    g.status,
                    g.outcome,
                    &repo.name,
                    &repo.git_ref.to_string(),
                    &g.msg,
                    g.step,
                    g.total,
                    g.started_at,
                    g.frozen_elapsed,
                    w,
                    "",
                ));
            } else {
                let label = format!("{}/{}", repo.name, g.name);
                lines.push(self.format_item_line(
                    g.status,
                    g.outcome,
                    &label,
                    &g.git_ref.to_string(),
                    &g.msg,
                    g.step,
                    g.total,
                    g.started_at,
                    g.frozen_elapsed,
                    w,
                    "",
                ));
            }
        } else {
            lines.push(self.format_item_line(
                repo.status,
                repo.outcome,
                &repo.name,
                &repo.git_ref.to_string(),
                &repo.msg,
                repo.step,
                repo.total,
                repo.started_at,
                repo.frozen_elapsed,
                w,
                "",
            ));
            for g in &repo_grammars {
                lines.push(self.format_item_line(
                    g.status,
                    g.outcome,
                    &g.name,
                    &g.git_ref.to_string(),
                    &g.msg,
                    g.step,
                    g.total,
                    g.started_at,
                    g.frozen_elapsed,
                    w,
                    "  ",
                ));
            }
        }

        lines
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
        status: ItemStatus,
        outcome: ItemOutcome,
        name: &str,
        git_ref: &str,
        msg: &str,
        step: usize,
        total: usize,
        started_at: Instant,
        frozen_elapsed: Option<Duration>,
        width: usize,
        indent: &str,
    ) -> Line<'static> {
        let terminal = status.is_terminal();
        let palette = palette_color(status, outcome);
        let name_style = Style::default().fg(palette).add_modifier(Modifier::BOLD);
        let progress_style = Style::default().fg(if terminal { palette } else { Color::White });
        let ref_style = Style::default().fg(Color::White);
        let msg_style = Style::default().fg(Color::White);

        let elapsed = frozen_elapsed.unwrap_or_else(|| started_at.elapsed());
        let time = format_elapsed_duration(elapsed);
        let time_col = format!("{time:<8}");
        let ref_col = format!("{:<12}", truncate_str(git_ref, 12));
        let step_col = if total > 0 {
            format!("{:>7}", format!("[{}/{}]", step.min(total), total))
        } else {
            "       ".to_string()
        };
        let indented_name = format!("{indent}{name}");
        let name_col = format!("{:<28}", truncate_str(&indented_name, 28));

        let fixed_width = 8 + 1 + 12 + 1 + 7 + 1 + 2 + 1 + 28 + 1;
        let msg_width = width.saturating_sub(fixed_width);
        let msg_col = truncate_str(msg, msg_width.max(1));

        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(time_col, progress_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(ref_col, ref_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(step_col, progress_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(status.icon(), progress_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(name_col, name_style));
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
        let (cached, built, building, failed) = self.grammar_status_counts();

        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(
            format!("✓ {cached} cached  "),
            Style::default().fg(if cached > 0 {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
        ));
        spans.push(Span::styled(
            format!("● {built} built  "),
            Style::default().fg(if built > 0 {
                Color::Blue
            } else {
                Color::DarkGray
            }),
        ));
        spans.push(Span::styled(
            format!("● {building} building  "),
            Style::default().fg(if building > 0 {
                Color::White
            } else {
                Color::DarkGray
            }),
        ));
        spans.push(Span::styled(
            format!("✗ {failed} failed"),
            Style::default().fg(if failed > 0 {
                Color::Red
            } else {
                Color::DarkGray
            }),
        ));

        Line::from(spans)
    }

    fn grammar_status_counts(&self) -> (usize, usize, usize, usize) {
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
