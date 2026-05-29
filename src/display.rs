//! Display state model: repos, grammars, grid cache, layout computation, and
//! summary counts.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap_verbosity_flag::{InfoLevel, Verbosity};
use log::Level;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::args::ProgressStyle;
use crate::git;

// ============================================================
// Constants
// ============================================================

/// Width of the icon column (●, ✓, ✗ are all single-width).
const ICON_COL_WIDTH: usize = 1;
/// Minimum width for the message column.
const MIN_MSG_WIDTH: usize = 1;
/// Minimum width of the name column (left-aligned).
const MIN_NAME_WIDTH: usize = 4;
/// Minimum width of the git-ref column (left-aligned).
const MIN_REF_WIDTH: usize = 4;
/// Minimum width of the step column (right-aligned, e.g. "[1/4]").
const MIN_STEP_WIDTH: usize = 5;
const MSG_STYLE: Style = Style::new();
const REF_STYLE: Style = Style::new();
/// Width of a spacer between columns (a single space).
const SPACER_WIDTH: usize = 1;
/// Fixed width of the time column (right-aligned, " 0.00s" … "59:59").
const TIME_COL_WIDTH: usize = 6;
const TIME_STYLE: Style = Style::new();

// ============================================================
// Enums
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Column {
  /// Icon (status indicator).
  Icon,
  /// Message text.
  Msg,
  /// Parser/grammar name.
  Name,
  /// Git ref string.
  Ref,
  /// Step counter (e.g. "[1/4]").
  Step,
  /// Elapsed time.
  Time,
}

pub(crate) enum ItemInfo<'a> {
  /// References a grammar-level entry.
  Grammar(&'a GrammarEntry),
  /// References a repo-level entry.
  Repo(&'a RepoEntry),
}

/// Complete lifecycle state for a display row.
///
/// This intentionally keeps terminality and success classification in one
/// field, so a row cannot be both "cancelled" and "still building".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
  /// Just registered, not yet started.
  New,
  /// In progress. The optional outcome is known after cache classification:
  /// cached rows stay live while they are re-installed, and built rows stay
  /// live while they are generated/built/installed.
  InProgress(Option<SuccessOutcome>),
  /// Successfully completed.
  Done(SuccessOutcome),
  /// Cancelled by shutdown.
  Cancelled,
  /// Failed.
  Failed,
}

/// The selected display backend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
  /// Ratatui inline terminal rendering.
  Fancy,
  /// Deterministic line-by-line output.
  Plain,
}

// ============================================================
// Structs
// ============================================================

/// The kind of rows: collapsed single-grammar repos and multi-grammar repos.
#[derive(Debug, Clone)]
pub(crate) enum RowKind {
  /// A grammar-level progress row.
  Grammar,
  /// A repo-level progress row.
  Repo,
}

/// Whether a completed build needed actual work or was served from cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuccessOutcome {
  /// Not cached; work was needed.
  Built,
  /// Cache hit.
  Cached,
}

// ============================================================
// Structs
// ============================================================

#[derive(Debug, Clone)]
pub(crate) struct CachedLayout {
  /// Width of the time column.
  pub time: usize,
  /// Width of the git-ref column.
  pub ref_: usize,
  /// Width of the step column.
  pub step: usize,
  /// Width of the name column.
  pub name: usize,
  /// Total width consumed by non-message columns: time + sp + ref + sp +
  /// step + sp + icon(1) + sp + name + sp.
  pub fixed_width: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CellKey {
  item_id: ItemId,
  column: Column,
}

#[derive(Debug, Clone)]
pub(crate) struct GrammarEntry {
  /// Parent repo name.
  pub repo: Arc<str>,
  /// Parent repo's item ID (for batch updates).
  pub repo_id: Option<ItemId>,
  /// Grammar name within the repo.
  pub name: Arc<str>,
  /// Git ref for display.
  pub git_ref: git::Ref,
  /// Current lifecycle state.
  pub state: ItemState,
  /// Current status message.
  pub msg: Arc<str>,
  /// Current step index (0-based).
  pub step: usize,
  /// Total number of steps.
  pub total: usize,
  /// Creation time for the whole item. This is not reset between steps.
  pub started_at: Instant,
  /// Frozen whole-item elapsed time for terminal states.
  pub frozen_elapsed: Option<Duration>,
}

pub(crate) struct GridCache {
  /// Cached rendered cells, keyed by (`item_id`, column).
  cells: HashMap<CellKey, Span<'static>>,
  /// Item IDs whose cells need recomputation.
  pub dirty_items: HashSet<ItemId>,
  /// Current column layout (widths).
  pub layout: CachedLayout,
}

/// Unique identifier for a display row (repo or grammar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemId(NonZeroU64);

#[derive(Debug, Clone)]
pub(crate) struct RepoEntry {
  /// Repo name.
  pub name: Arc<str>,
  /// Git ref for display.
  pub git_ref: git::Ref,
  /// Current lifecycle state.
  pub state: ItemState,
  /// Current status message.
  pub msg: Arc<str>,
  /// Current step index (0-based).
  pub step: usize,
  /// Total number of steps.
  pub total: usize,
  /// Creation time for the whole item. This is not reset between steps.
  pub started_at: Instant,
  /// Frozen whole-item elapsed time for terminal states.
  pub frozen_elapsed: Option<Duration>,
}

#[derive(Debug, Clone)]
pub(crate) struct RowSpec {
  /// Unique row identifier.
  pub id: ItemId,
  /// Whether this row is a repo or a grammar.
  pub kind: RowKind,
  /// The text to display in the name column (already includes indent).
  pub display_name: Arc<str>,
  /// Indentation prefix for the name column (applied before padding).
  pub indent: &'static str,
}

/// Tallies of terminal row states for the summary footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct BuildSummary {
  /// Rows still building.
  pub building: usize,
  /// Rows that were built (not cached).
  pub built: usize,
  /// Rows served from cache.
  pub cached: usize,
  /// Rows cancelled by shutdown.
  pub cancelled: usize,
  /// Rows that failed.
  pub failed: usize,
}

pub(crate) struct State {
  /// Build directory root path.
  pub build_dir: PathBuf,
  footer_build: Line<'static>,
  footer_out: Line<'static>,
  /// Grammar-level entries keyed by item ID.
  pub grammars: HashMap<ItemId, GrammarEntry>,
  /// Display mode.
  pub mode: Mode,
  /// Output directory path.
  pub out_dir: PathBuf,
  /// Repo-level entries keyed by item ID.
  pub repos: HashMap<ItemId, RepoEntry>,
}

// ============================================================
// Free functions
// ============================================================

/// Render the status icon cell (●, ✓, ✗) with indicator color.
pub(crate) fn compute_icon_cell(info: &ItemInfo<'_>) -> Span<'static> {
  let state = info.state();
  Span::styled(
    state.icon(),
    Style::default()
      .fg(state.indicator_color())
      .add_modifier(Modifier::BOLD),
  )
}

/// Render the message cell, truncating to fit the available width.
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

/// Render the name cell with indentation and outcome-based colour.
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
      .fg(info.state().name_color())
      .add_modifier(Modifier::BOLD),
  )
}

/// Render the git ref cell, left-aligned.
pub(crate) fn compute_ref_cell(info: &ItemInfo<'_>, layout: &CachedLayout) -> Span<'static> {
  let ref_str = info.git_ref().short();
  let padded = format!("{:<width$}", ref_str, width = layout.ref_);
  Span::styled(padded, REF_STYLE)
}

/// Render the step counter cell (e.g. "[2/4]"), right-aligned.
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

/// Render the elapsed time cell, right-aligned.
pub(crate) fn compute_time_cell(info: &ItemInfo<'_>, layout: &CachedLayout) -> Span<'static> {
  let time = format_elapsed_duration(info.elapsed());
  // Right-aligned
  let padded = format!("{:>width$}", time, width = layout.time);
  Span::styled(padded, TIME_STYLE)
}

/// Accumulate a single item state into the summary counters.
/// Accumulate a single item state into the summary counters.
fn count_item_state(
  state: ItemState,
  cached: &mut usize,
  built: &mut usize,
  building: &mut usize,
  failed: &mut usize,
  cancelled: &mut usize,
) {
  match state {
    ItemState::New | ItemState::InProgress(_) => *building += 1,
    ItemState::Done(SuccessOutcome::Cached) => *cached += 1,
    ItemState::Done(SuccessOutcome::Built) => *built += 1,
    ItemState::Cancelled => *cancelled += 1,
    ItemState::Failed => *failed += 1,
  }
}

/// Pre-computed dimmed footer lines (static; never change).
/// Build a dimmed footer line.
fn dim_line(text: String) -> Line<'static> {
  Line::from(Span::styled(
    text,
    Style::default()
      .fg(Color::DarkGray)
      .add_modifier(Modifier::DIM),
  ))
}

/// Pre-compute the fixed (non-message) portion of a row line.
///
/// Columns in order: Time _ Ref _ Step _ Icon _ Name _ Msg
/// (5 spacers between 6 columns).
/// Compute the total width consumed by all non-message columns.
const fn fixed_width(time: usize, rf: usize, step: usize, name: usize) -> usize {
  time
    + SPACER_WIDTH
    + rf
    + SPACER_WIDTH
    + step
    + SPACER_WIDTH
    + ICON_COL_WIDTH
    + SPACER_WIDTH
    + name
    + SPACER_WIDTH
}

/// Format a duration as " 0.00s" or " 3:45".
/// Format a duration as " 0.00s" or " 3:45".
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

/// Append a "building" counter span to the summary line.
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

/// Append a "cancelled" counter span to the summary line.
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

/// Append a "failed" counter span to the summary line.
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

/// Append a "cached"/"built" success counter span to the summary line.
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

/// Truncate a string with an ellipsis if it exceeds `max` characters.
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

// ============================================================
// Impls
// ============================================================

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
  /// Apply minimum width constraints and recompute `fixed_width`.
  fn finalize(&mut self) {
    self.time = TIME_COL_WIDTH;
    self.ref_ = self.ref_.max(MIN_REF_WIDTH);
    self.step = self.step.max(MIN_STEP_WIDTH);
    self.name = self.name.max(MIN_NAME_WIDTH);
    self.fixed_width = fixed_width(self.time, self.ref_, self.step, self.name);
  }
}

impl GrammarEntry {
  /// Return the elapsed time (frozen or live).
  pub(crate) fn elapsed(&self) -> Duration {
    self
      .frozen_elapsed
      .unwrap_or_else(|| self.started_at.elapsed())
  }
}

impl GridCache {
  /// Create an empty grid cache.
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
    item_id: ItemId,
    column: Column,
    compute: impl FnOnce() -> Span<'static>,
    stale: bool,
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

  /// Mark all cells for an item as needing recomputation.
  pub fn mark_dirty(&mut self, item_id: ItemId) {
    self.dirty_items.insert(item_id);
  }

  /// Clear the dirty set after recomputation.
  pub fn clear_dirty(&mut self) {
    self.dirty_items.clear();
  }

  /// Invalidate all cached cells for a given column (e.g. when layout width
  /// for that column changes).
  pub fn invalidate_column(&mut self, column: Column) {
    self.cells.retain(|key, _| key.column != column);
  }
}

impl ItemId {
  /// Create an `ItemId` from a non-zero u64.
  #[must_use]
  pub(crate) fn new(value: NonZeroU64) -> Self {
    Self(value)
  }

  /// Return the next sequential `ItemId`.
  #[must_use]
  pub(crate) fn next_after(self) -> Self {
    let next = self
      .0
      .get()
      .checked_add(1)
      .and_then(NonZeroU64::new)
      .expect("display item id exhausted");
    Self(next)
  }
}

impl ItemInfo<'_> {
  /// Get the current item state.
  pub(crate) fn state(&self) -> ItemState {
    match self {
      ItemInfo::Grammar(g) => g.state,
      ItemInfo::Repo(r) => r.state,
    }
  }

  /// Get the current message string.
  pub(crate) fn msg(&self) -> &str {
    match self {
      ItemInfo::Grammar(g) => &g.msg,
      ItemInfo::Repo(r) => &r.msg,
    }
  }

  /// Get the current step index (0-based).
  pub(crate) fn step(&self) -> usize {
    match self {
      ItemInfo::Grammar(g) => g.step,
      ItemInfo::Repo(r) => r.step,
    }
  }

  /// Get the total number of steps.
  pub(crate) fn total(&self) -> usize {
    match self {
      ItemInfo::Grammar(g) => g.total,
      ItemInfo::Repo(r) => r.total,
    }
  }

  /// Get the git ref for this item.
  /// Get the git ref for this item.
  pub(crate) fn git_ref(&self) -> &git::Ref {
    match self {
      ItemInfo::Grammar(g) => &g.git_ref,
      ItemInfo::Repo(r) => &r.git_ref,
    }
  }

  /// Get the elapsed time for this item.
  pub(crate) fn elapsed(&self) -> Duration {
    match self {
      ItemInfo::Grammar(g) => g.elapsed(),
      ItemInfo::Repo(r) => r.elapsed(),
    }
  }
}

impl ItemState {
  /// Check whether the item is still live (new or in progress).
  #[must_use]
  pub fn is_live(self) -> bool {
    matches!(self, Self::New | Self::InProgress(_))
  }

  /// Get the success outcome, if the item has reached a terminal outcome state.
  #[must_use]
  pub fn success_outcome(self) -> Option<SuccessOutcome> {
    match self {
      Self::InProgress(outcome) => outcome,
      Self::Done(outcome) => Some(outcome),
      Self::New | Self::Cancelled | Self::Failed => None,
    }
  }

  /// Return the status icon character.
  fn icon(self) -> &'static str {
    match self {
      Self::New | Self::InProgress(_) => "●",
      Self::Done(_) => "✓",
      Self::Cancelled | Self::Failed => "✗",
    }
  }

  /// Return the colour for the name text.
  fn name_color(self) -> Color {
    match self {
      Self::Failed => Color::Red,
      Self::Cancelled => Color::Yellow,
      Self::New | Self::InProgress(None) => Color::DarkGray,
      Self::InProgress(Some(outcome)) | Self::Done(outcome) => outcome.color(),
    }
  }

  /// Return the colour for the status indicator.
  fn indicator_color(self) -> Color {
    match self {
      Self::Done(_) => Color::Green,
      Self::Cancelled => Color::Yellow,
      Self::Failed => Color::Red,
      Self::New | Self::InProgress(_) => Color::DarkGray,
    }
  }
}

impl RepoEntry {
  /// Return the elapsed time (frozen or live).
  pub(crate) fn elapsed(&self) -> Duration {
    self
      .frozen_elapsed
      .unwrap_or_else(|| self.started_at.elapsed())
  }
}

impl State {
  /// Create a new display state with the given build dir, mode, and output dir.
  pub fn new(build_dir: PathBuf, mode: Mode, out_dir: PathBuf) -> Self {
    let footer_build = dim_line(format!("build: {}", build_dir.display()));
    let footer_out = dim_line(format!("out:   {}", out_dir.display()));
    Self {
      build_dir,
      footer_build,
      footer_out,
      grammars: HashMap::new(),
      mode,
      out_dir,
      repos: HashMap::new(),
    }
  }

  // ── Footer lines ──────────────────────────────────────────────

  /// Return the pre-computed "build:" footer line.
  pub fn footer_build_line(&self) -> Line<'static> {
    self.footer_build.clone()
  }

  /// Return the pre-computed "out:" footer line.
  pub fn footer_out_line(&self) -> Line<'static> {
    self.footer_out.clone()
  }

  /// Build the summary counts footer line.
  pub fn format_footer_counts(&self) -> Line<'static> {
    let s = self.summary();
    let mut spans: Vec<Span<'static>> = Vec::new();

    push_summary_success(&mut spans, s.cached, "cached", Color::Yellow, true);
    push_summary_success(&mut spans, s.built, "built", Color::Blue, false);
    if s.cancelled > 0 {
      push_summary_cancelled(&mut spans, s.cancelled, false);
    }
    if s.building > 0 {
      push_summary_building(&mut spans, s.building);
    }
    push_summary_failed(&mut spans, s.failed);

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
    let mut sorted_repos: Vec<(ItemId, &RepoEntry)> =
      self.repos.iter().map(|(id, r)| (*id, r)).collect();
    sorted_repos.sort_by(|(_, a), (_, b)| a.name.cmp(&b.name));

    // Gather grammar (id, entry) pairs grouped by repo, sorted by name
    let mut grammars_by_repo: HashMap<ItemId, Vec<(ItemId, &GrammarEntry)>> = HashMap::new();
    for (gid, g) in &self.grammars {
      if let Some(repo_id) = g.repo_id {
        grammars_by_repo.entry(repo_id).or_default().push((*gid, g));
      }
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

  /// Get the item info for a row spec.
  pub fn get_item_info(&self, spec: &RowSpec) -> ItemInfo<'_> {
    match spec.kind {
      RowKind::Grammar => ItemInfo::Grammar(
        self
          .grammars
          .get(&spec.id)
          .expect("grammar not found for row spec"),
      ),
      RowKind::Repo => ItemInfo::Repo(
        self
          .repos
          .get(&spec.id)
          .expect("repo not found for row spec"),
      ),
    }
  }

  // ── Layout ────────────────────────────────────────────────────

  /// Compute column layout from the current row specs and state.
  pub fn compute_layout(&self, row_specs: &[RowSpec]) -> CachedLayout {
    let mut layout = CachedLayout::default();
    for spec in row_specs {
      let info = self.get_item_info(spec);
      let ref_str = info.git_ref().short();
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

  /// Compute build summary counts from all item states.
  pub(crate) fn summary(&self) -> BuildSummary {
    let mut cached = 0;
    let mut built = 0;
    let mut building = 0;
    let mut failed = 0;
    let mut cancelled = 0;

    let mut repos_with_grammars: HashSet<ItemId> = HashSet::new();
    for grammar in self.grammars.values() {
      if let Some(repo_id) = grammar.repo_id {
        repos_with_grammars.insert(repo_id);
      }
      count_item_state(
        grammar.state,
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
        count_item_state(
          repo.state,
          &mut cached,
          &mut built,
          &mut building,
          &mut failed,
          &mut cancelled,
        );
      }
    }

    BuildSummary {
      building,
      built,
      cached,
      cancelled,
      failed,
    }
  }
}

impl SuccessOutcome {
  /// Return the display colour for entries with this outcome.
  fn color(self) -> Color {
    match self {
      SuccessOutcome::Built => Color::Blue,
      SuccessOutcome::Cached => Color::Yellow,
    }
  }
}
