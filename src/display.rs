use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap_verbosity_flag::{InfoLevel, Verbosity};
use log::Level;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::args::ProgressStyle;
use crate::git::GitRef;

// ── Column geometry ──────────────────────────────────────────────
/// Fixed width of the time column (right-aligned, " 0.00s" … "59:59").
const TIME_COL_WIDTH: usize = 6;
/// Minimum width of the git-ref column (left-aligned).
const MIN_REF_WIDTH: usize = 4;
/// Minimum width of the step column (right-aligned, e.g. "[1/4]").
const MIN_STEP_WIDTH: usize = 5;
/// Minimum width of the name column (left-aligned).
const MIN_NAME_WIDTH: usize = 4;
/// Width of the icon column (●, ✓, ✗ are all single-width).
const ICON_COL_WIDTH: usize = 1;
/// Width of a spacer between columns (a single space).
const SPACER_WIDTH: usize = 1;
/// Minimum width for the message column.
const MIN_MSG_WIDTH: usize = 1;

/// Pre-compute the fixed (non-message) portion of a row line.
///
/// Columns in order: Time _ Ref _ Step _ Icon _ Name _ Msg
/// (5 spacers between 6 columns).
const fn fixed_width(time: usize, rf: usize, step: usize, name: usize) -> usize {
    time + SPACER_WIDTH
        + rf
        + SPACER_WIDTH
        + step
        + SPACER_WIDTH
        + ICON_COL_WIDTH
        + SPACER_WIDTH
        + name
        + SPACER_WIDTH
}

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
    /// Cancelled by shutdown.
    Cancelled,
    /// Failed.
    Failed,
}

impl ItemStatus {
    fn icon(self) -> &'static str {
        match self {
            ItemStatus::New | ItemStatus::InProgress => "●",
            ItemStatus::Done => "✓",
            ItemStatus::Cancelled | ItemStatus::Failed => "✗",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOutcome {
    /// We do not yet know whether this item is cached or needs work.
    Unknown,
    /// Cache hit.
    Cached,
    /// Shut down before completion.
    Cancelled,
    /// Not cached; work was needed.
    Built,
}

impl ItemOutcome {
    fn color(self) -> Color {
        match self {
            ItemOutcome::Unknown | ItemOutcome::Cancelled => Color::DarkGray,
            ItemOutcome::Cached => Color::Yellow,
            ItemOutcome::Built => Color::Blue,
        }
    }
}

fn name_color(status: ItemStatus, outcome: ItemOutcome) -> Color {
    match status {
        ItemStatus::Failed => Color::Red,
        ItemStatus::Cancelled => Color::Yellow,
        ItemStatus::New | ItemStatus::InProgress | ItemStatus::Done => outcome.color(),
    }
}

fn indicator_color(status: ItemStatus) -> Color {
    match status {
        ItemStatus::Done => Color::Green,
        ItemStatus::Cancelled => Color::Yellow,
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

impl RepoEntry {
    pub(crate) fn elapsed(&self) -> Duration {
        self.frozen_elapsed
            .unwrap_or_else(|| self.started_at.elapsed())
    }
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

impl GrammarEntry {
    pub(crate) fn elapsed(&self) -> Duration {
        self.frozen_elapsed
            .unwrap_or_else(|| self.started_at.elapsed())
    }
}

// ---------------------------------------------------------------------------
// Column identifiers for the grid cache
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Column {
    Time,
    GitRef,
    Step,
    Icon,
    Name,
    Msg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CellKey {
    item_id: u64,
    column: Column,
}

// ---------------------------------------------------------------------------
// Cached layout (column widths)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct CachedLayout {
    pub time: usize,
    pub ref_: usize,
    pub step: usize,
    pub name: usize,
    /// Total width consumed by non-message columns: time + sp + ref + sp +
    /// step + sp + icon(1) + sp + name + sp.
    pub fixed_width: usize,
}

impl Default for CachedLayout {
    fn default() -> Self {
        Self {
            time: TIME_COL_WIDTH,
            ref_: MIN_REF_WIDTH,
            step: MIN_STEP_WIDTH,
            name: MIN_NAME_WIDTH,
            fixed_width: fixed_width(
                TIME_COL_WIDTH,
                MIN_REF_WIDTH,
                MIN_STEP_WIDTH,
                MIN_NAME_WIDTH,
            ),
        }
    }
}

impl CachedLayout {
    fn finalize(&mut self) {
        self.time = TIME_COL_WIDTH;
        self.ref_ = self.ref_.max(MIN_REF_WIDTH);
        self.step = self.step.max(MIN_STEP_WIDTH);
        self.name = self.name.max(MIN_NAME_WIDTH);
        self.fixed_width = fixed_width(self.time, self.ref_, self.step, self.name);
    }
}

// ---------------------------------------------------------------------------
// Grid cache
// ---------------------------------------------------------------------------

pub(crate) struct GridCache {
    cells: HashMap<CellKey, Span<'static>>,
    pub dirty_items: HashSet<u64>,
    pub layout: CachedLayout,
}

impl GridCache {
    pub fn new() -> Self {
        Self {
            cells: HashMap::new(),
            dirty_items: HashSet::new(),
            layout: CachedLayout::default(),
        }
    }

    /// Get a cell for the given item and column. If `stale` is true (or the
    /// cell is not cached), `compute` is called, the result is cached, and
    /// returned. Otherwise the cached cell is cloned.
    pub fn cell(
        &mut self,
        item_id: u64,
        column: Column,
        stale: bool,
        compute: impl FnOnce() -> Span<'static>,
    ) -> Span<'static> {
        let key = CellKey { item_id, column };
        if stale {
            let span = compute();
            self.cells.insert(key, span.clone());
            span
        } else if let Some(cached) = self.cells.get(&key) {
            cached.clone()
        } else {
            let span = compute();
            self.cells.insert(key, span.clone());
            span
        }
    }

    pub fn mark_dirty(&mut self, item_id: u64) {
        self.dirty_items.insert(item_id);
    }

    pub fn clear_dirty(&mut self) {
        self.dirty_items.clear();
    }

    /// Invalidate all cached cells for a given column (e.g. when layout width
    /// for that column changes).
    pub fn invalidate_column(&mut self, column: Column) {
        self.cells.retain(|key, _| key.column != column);
    }
}

// ---------------------------------------------------------------------------
// Row specification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) enum RowKind {
    Repo,
    Grammar,
}

#[derive(Debug, Clone)]
pub(crate) struct RowSpec {
    pub id: u64,
    pub kind: RowKind,
    /// The text to display in the name column (already includes indent).
    pub display_name: Arc<str>,
    /// Indentation prefix for the name column (applied before padding).
    pub indent: &'static str,
}

// ---------------------------------------------------------------------------
// Unified item info (abstracts over RepoEntry / GrammarEntry)
// ---------------------------------------------------------------------------

pub(crate) enum ItemInfo<'a> {
    Repo(&'a RepoEntry),
    Grammar(&'a GrammarEntry),
}

impl ItemInfo<'_> {
    pub(crate) fn status(&self) -> ItemStatus {
        match self {
            ItemInfo::Repo(r) => r.status,
            ItemInfo::Grammar(g) => g.status,
        }
    }

    pub(crate) fn outcome(&self) -> ItemOutcome {
        match self {
            ItemInfo::Repo(r) => r.outcome,
            ItemInfo::Grammar(g) => g.outcome,
        }
    }

    pub(crate) fn msg(&self) -> &str {
        match self {
            ItemInfo::Repo(r) => &r.msg,
            ItemInfo::Grammar(g) => &g.msg,
        }
    }

    pub(crate) fn step(&self) -> usize {
        match self {
            ItemInfo::Repo(r) => r.step,
            ItemInfo::Grammar(g) => g.step,
        }
    }

    pub(crate) fn total(&self) -> usize {
        match self {
            ItemInfo::Repo(r) => r.total,
            ItemInfo::Grammar(g) => g.total,
        }
    }

    pub(crate) fn git_ref(&self) -> &GitRef {
        match self {
            ItemInfo::Repo(r) => &r.git_ref,
            ItemInfo::Grammar(g) => &g.git_ref,
        }
    }

    pub(crate) fn elapsed(&self) -> Duration {
        match self {
            ItemInfo::Repo(r) => r.elapsed(),
            ItemInfo::Grammar(g) => g.elapsed(),
        }
    }
}

// ---------------------------------------------------------------------------
// Cell factories — each produces a fully padded, fully styled Span
// ---------------------------------------------------------------------------

const TIME_STYLE: Style = Style::new();
const REF_STYLE: Style = Style::new();
const MSG_STYLE: Style = Style::new();

pub(crate) fn compute_time_cell(info: &ItemInfo<'_>, layout: &CachedLayout) -> Span<'static> {
    let time = format_elapsed_duration(info.elapsed());
    // Right-aligned
    let padded = format!("{:>width$}", time, width = layout.time);
    Span::styled(padded, TIME_STYLE)
}

pub(crate) fn compute_ref_cell(info: &ItemInfo<'_>, layout: &CachedLayout) -> Span<'static> {
    let ref_str = info.git_ref().to_string();
    let padded = format!("{:<width$}", ref_str, width = layout.ref_);
    Span::styled(padded, REF_STYLE)
}

pub(crate) fn compute_step_cell(info: &ItemInfo<'_>, layout: &CachedLayout) -> Span<'static> {
    let step_str = if info.total() > 0 {
        format!("[{}/{}]", info.step().min(info.total()), info.total())
    } else {
        String::new()
    };
    // Right-aligned
    let padded = format!("{:>width$}", step_str, width = layout.step);
    Span::styled(padded, REF_STYLE)
}

pub(crate) fn compute_icon_cell(info: &ItemInfo<'_>) -> Span<'static> {
    let status = info.status();
    Span::styled(
        status.icon(),
        Style::default()
            .fg(indicator_color(status))
            .add_modifier(Modifier::BOLD),
    )
}

pub(crate) fn compute_name_cell(
    display_name: &str,
    indent: &str,
    info: &ItemInfo<'_>,
    layout: &CachedLayout,
) -> Span<'static> {
    let full = format!("{indent}{display_name}");
    let padded = format!("{:<width$}", full, width = layout.name);
    Span::styled(
        padded,
        Style::default()
            .fg(name_color(info.status(), info.outcome()))
            .add_modifier(Modifier::BOLD),
    )
}

pub(crate) fn compute_msg_cell(
    info: &ItemInfo<'_>,
    layout: &CachedLayout,
    term_width: usize,
) -> Span<'static> {
    let msg_width = term_width
        .saturating_sub(layout.fixed_width)
        .max(MIN_MSG_WIDTH);
    Span::styled(truncate_str(info.msg(), msg_width), MSG_STYLE)
}

// ---------------------------------------------------------------------------
// DisplayState — the source of truth
// ---------------------------------------------------------------------------

/// Pre-computed dimmed footer lines (static; never change).
fn dim_line(text: String) -> Line<'static> {
    Line::from(Span::styled(
        text,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    ))
}

pub(crate) struct DisplayState {
    pub mode: Mode,
    pub repos: HashMap<u64, RepoEntry>,
    pub grammars: HashMap<u64, GrammarEntry>,
    pub build_dir: Arc<PathBuf>,
    pub out_dir: Arc<PathBuf>,
    footer_build: Line<'static>,
    footer_out: Line<'static>,
}

impl DisplayState {
    pub fn new(mode: Mode, build_dir: Arc<PathBuf>, out_dir: Arc<PathBuf>) -> Self {
        let footer_build = dim_line(format!("build: {}", build_dir.display()));
        let footer_out = dim_line(format!("out:   {}", out_dir.display()));
        Self {
            mode,
            repos: HashMap::new(),
            grammars: HashMap::new(),
            build_dir,
            out_dir,
            footer_build,
            footer_out,
        }
    }

    // ── Footer lines ──────────────────────────────────────────────

    pub fn footer_build_line(&self) -> Line<'static> {
        self.footer_build.clone()
    }

    pub fn footer_out_line(&self) -> Line<'static> {
        self.footer_out.clone()
    }

    pub fn format_footer_counts(&self) -> Line<'static> {
        let (cached, built, building, failed, cancelled) = self.summary_counts();
        let mut spans: Vec<Span<'static>> = Vec::new();

        push_summary_success(&mut spans, cached, "cached", Color::Yellow, true);
        push_summary_success(&mut spans, built, "built", Color::Blue, false);
        if cancelled > 0 {
            push_summary_cancelled(&mut spans, cancelled, false);
        }
        if building > 0 {
            push_summary_building(&mut spans, building);
        }
        push_summary_failed(&mut spans, failed);

        Line::from(spans)
    }

    // ── Row order ─────────────────────────────────────────────────

    /// Compute the sorted row order. Repos are sorted alphabetically by name;
    /// grammars within each repo are sorted alphabetically by name.
    /// Collapses single-grammar repos where the grammar name equals the repo
    /// name into a single grammar row (no separate repo row).
    pub fn compute_row_order(&self) -> Vec<RowSpec> {
        let mut rows = Vec::new();

        // Sort repos alphabetically by name
        let mut sorted_repos: Vec<(u64, &RepoEntry)> =
            self.repos.iter().map(|(id, r)| (*id, r)).collect();
        sorted_repos.sort_by(|(_, a), (_, b)| a.name.cmp(&b.name));

        // Gather grammar (id, entry) pairs grouped by repo, sorted by name
        let mut grammars_by_repo: HashMap<u64, Vec<(u64, &GrammarEntry)>> = HashMap::new();
        for (gid, g) in &self.grammars {
            grammars_by_repo
                .entry(g.repo_id)
                .or_default()
                .push((*gid, g));
        }
        for list in grammars_by_repo.values_mut() {
            list.sort_by(|(_, a), (_, b)| a.name.cmp(&b.name));
        }

        for (repo_id, repo) in &sorted_repos {
            let repo_grammars = grammars_by_repo.remove(repo_id).unwrap_or_default();

            if repo_grammars.is_empty() {
                rows.push(RowSpec {
                    id: *repo_id,
                    kind: RowKind::Repo,
                    display_name: repo.name.clone(),
                    indent: "",
                });
            } else if repo_grammars.len() == 1 {
                let (gid, grammar) = repo_grammars[0];
                let display_name: Arc<str> = if grammar.name == repo.name {
                    repo.name.clone()
                } else {
                    format!("{}/{}", repo.name, grammar.name).into()
                };
                rows.push(RowSpec {
                    id: gid,
                    kind: RowKind::Grammar,
                    display_name,
                    indent: "",
                });
            } else {
                rows.push(RowSpec {
                    id: *repo_id,
                    kind: RowKind::Repo,
                    display_name: repo.name.clone(),
                    indent: "",
                });
                for (gid, grammar) in repo_grammars {
                    rows.push(RowSpec {
                        id: gid,
                        kind: RowKind::Grammar,
                        display_name: grammar.name.clone(),
                        indent: "  ",
                    });
                }
            }
        }

        rows
    }

    // ── Item access ───────────────────────────────────────────────

    pub fn get_item_info(&self, spec: &RowSpec) -> ItemInfo<'_> {
        match spec.kind {
            RowKind::Repo => ItemInfo::Repo(
                self.repos
                    .get(&spec.id)
                    .expect("repo not found for row spec"),
            ),
            RowKind::Grammar => ItemInfo::Grammar(
                self.grammars
                    .get(&spec.id)
                    .expect("grammar not found for row spec"),
            ),
        }
    }

    // ── Layout ────────────────────────────────────────────────────

    /// Compute column layout from the current row specs and state.
    pub fn compute_layout(&self, row_specs: &[RowSpec]) -> CachedLayout {
        let mut layout = CachedLayout::default();
        for spec in row_specs {
            let info = self.get_item_info(spec);
            let ref_str = info.git_ref().to_string();
            let step_str = if info.total() > 0 {
                format!("[{}/{}]", info.step().min(info.total()), info.total())
            } else {
                String::new()
            };
            let name_full = format!("{}{}", spec.indent, spec.display_name);
            layout.ref_ = layout.ref_.max(ref_str.chars().count());
            layout.step = layout.step.max(step_str.chars().count());
            layout.name = layout.name.max(name_full.chars().count());
        }
        layout.finalize();
        layout
    }

    // ── Summary counts ────────────────────────────────────────────

    pub(crate) fn summary_counts(&self) -> (usize, usize, usize, usize, usize) {
        let mut cached = 0;
        let mut built = 0;
        let mut building = 0;
        let mut failed = 0;
        let mut cancelled = 0;

        let mut repos_with_grammars: HashSet<u64> = HashSet::new();
        for grammar in self.grammars.values() {
            repos_with_grammars.insert(grammar.repo_id);
            count_grammar_status(
                grammar.status,
                grammar.outcome,
                &mut cached,
                &mut built,
                &mut building,
                &mut failed,
                &mut cancelled,
            );
        }

        // Repos without grammars are visible terminal rows too (for example,
        // tree-sitter-cli or a language that failed before grammar discovery).
        for (repo_id, repo) in &self.repos {
            if !repos_with_grammars.contains(repo_id) {
                count_grammar_status(
                    repo.status,
                    repo.outcome,
                    &mut cached,
                    &mut built,
                    &mut building,
                    &mut failed,
                    &mut cancelled,
                );
            }
        }

        (cached, built, building, failed, cancelled)
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
    cancelled: &mut usize,
) {
    match status {
        ItemStatus::New | ItemStatus::InProgress => *building += 1,
        ItemStatus::Done => match outcome {
            ItemOutcome::Cached => *cached += 1,
            ItemOutcome::Built | ItemOutcome::Unknown => *built += 1,
            ItemOutcome::Cancelled => *cancelled += 1,
        },
        ItemStatus::Cancelled => *cancelled += 1,
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
        format!("{secs:>5.2}s")
    } else {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mins = secs as u64 / 60;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let remaining = secs as u64 % 60;
        format!("{mins:>3}:{remaining:02}")
    }
}

fn push_summary_cancelled(spans: &mut Vec<Span<'static>>, count: usize, _leading: bool) {
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("✗ {count} cancelled"),
        Style::default().fg(if count > 0 {
            Color::Yellow
        } else {
            Color::DarkGray
        }),
    ));
}
