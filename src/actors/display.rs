use std::io;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::backend::Backend;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};
use tokio::sync::{mpsc, oneshot};
use tokio::time;
use tracing::error;

use crate::actors::Addr;
use crate::display;
use crate::git;

// ============================================================
// Enums
// ============================================================

#[derive(Debug)]
pub enum Message {
    /// Register a repo-level progress line. Returns a `ProgressAddr`.
    RegisterLanguage {
        git_ref: git::Ref,
        name: Arc<str>,
        num_tasks: usize,
        tx: oneshot::Sender<ProgressAddr>,
    },

    /// Register a grammar-level progress line. Returns a `ProgressAddr`.
    RegisterGrammar {
        git_ref: git::Ref,
        language: Arc<str>,
        name: Arc<str>,
        num_tasks: usize,
        tx: oneshot::Sender<ProgressAddr>,
    },

    /// Plain-mode reference line. Fancy mode ignores this because refs are rendered per row.
    RegisterReference { git_ref: git::Ref, name: Arc<str> },

    /// Update a specific bar.
    Update {
        id: display::ItemId,
        kind: UpdateKind,
        msg: Arc<str>,
    },

    /// Flush and close the display actor. The response is sent after cleanup.
    Shutdown {
        interrupted: bool,
        tx: oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum UpdateKind {
    Msg,
    Step,
    SetOutcomeCached,
    SetOutcomeBuilt,
    Cancel,
    Cached,
    Fin,
    Err,
}

// ============================================================
// Structs
// ============================================================

pub struct DisplayActor {
    state: display::State,
    next_id: display::ItemId,
    plain_name_width: usize,
    plain_progress_started: bool,
    grid: display::GridCache,
    row_specs: Vec<display::RowSpec>,
    rows_dirty: bool,
    last_term_width: Option<usize>,
    rx: mpsc::Receiver<Message>,
    tx: mpsc::Sender<Message>,
}

/// Handle for updating a specific progress bar (repo or grammar).
#[derive(Debug, Clone)]
pub struct DisplayAddr {
    tx: mpsc::Sender<Message>,
    #[allow(dead_code)]
    mode: display::Mode,
}

struct PlainLine {
    name: String,
    step: usize,
    total: usize,
    message: String,
}

/// Handle for updating a specific progress bar (repo or grammar).
#[derive(Debug, Clone)]
pub struct ProgressAddr {
    id: display::ItemId,
    tx: mpsc::Sender<Message>,
}

// ============================================================
// Free functions
// ============================================================

fn clear_from_cursor_down() -> io::Result<()> {
    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
    )
}

fn current_viewport_height<B: Backend>(
    terminal: &mut ratatui::Terminal<B>,
) -> Result<u16, B::Error> {
    terminal.autoresize()?;
    Ok(terminal.get_frame().area().height.max(1))
}

fn draw_lines<B: Backend>(
    terminal: &mut ratatui::Terminal<B>,
    lines: Vec<Line<'static>>,
) -> Result<(), B::Error> {
    terminal
        .draw(|frame| {
            let area = frame.area();
            frame.render_widget(Paragraph::new(lines), area);
        })
        .map(|_| ())
}

fn draw_lines_in_viewport<B: Backend>(
    terminal: &mut ratatui::Terminal<B>,
    lines: Vec<Line<'static>>,
) -> Result<(), B::Error> {
    let line_count = lines.len();

    terminal
        .draw(move |frame| {
            let area = frame.area();
            frame.render_widget(Clear, area);
            frame.render_widget(Paragraph::new(lines), area);

            let cursor_offset = line_count
                .saturating_sub(1)
                .min(usize::from(area.height.saturating_sub(1)));
            let cursor_y = area.y + u16::try_from(cursor_offset).unwrap_or(0);
            frame.set_cursor_position((area.x, cursor_y));
        })
        .map(|_| ())
}

fn finish_item(state: display::ItemState) -> Result<display::ItemState, String> {
    match state {
        display::ItemState::InProgress(Some(outcome)) => Ok(display::ItemState::Done(outcome)),
        display::ItemState::New | display::ItemState::InProgress(None) => invalid_finish(
            "finish update received before cached/built path was set",
            state,
        ),
        display::ItemState::Done(_)
        | display::ItemState::Cancelled
        | display::ItemState::Failed => {
            invalid_finish("finish update received for terminal item state", state)
        }
    }
}

fn insert_lines_before_viewport<B: Backend>(
    terminal: &mut ratatui::Terminal<B>,
    mut lines: Vec<Line<'static>>,
) -> Result<(), B::Error> {
    while !lines.is_empty() {
        let rest = if lines.len() > usize::from(u16::MAX) {
            lines.split_off(usize::from(u16::MAX))
        } else {
            Vec::new()
        };
        let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);

        terminal.insert_before(height, move |buf| {
            Paragraph::new(lines).render(buf.area, buf);
        })?;

        lines = rest;
    }

    Ok(())
}

fn invalid_finish(reason: &str, state: display::ItemState) -> Result<display::ItemState, String> {
    let message = format!("{reason}: {state:?}");
    error!("{message}");
    debug_assert!(
        matches!(state, display::ItemState::InProgress(Some(_))),
        "{message}"
    );
    Err(message)
}

fn mark_success(state: display::ItemState, outcome: display::SuccessOutcome) -> display::ItemState {
    match state {
        display::ItemState::New | display::ItemState::InProgress(_) => {
            display::ItemState::InProgress(Some(outcome))
        }
        display::ItemState::Done(_)
        | display::ItemState::Cancelled
        | display::ItemState::Failed => state,
    }
}

fn plain_grammar_message(kind: UpdateKind, state: display::ItemState, msg: &str) -> String {
    match kind {
        UpdateKind::Err => format!("failed: {msg}"),
        UpdateKind::Cached => "cached".to_string(),
        UpdateKind::Fin => match state {
            display::ItemState::Done(display::SuccessOutcome::Cached) => "cached".to_string(),
            display::ItemState::Done(display::SuccessOutcome::Built) => "built".to_string(),
            display::ItemState::New
            | display::ItemState::InProgress(_)
            | display::ItemState::Cancelled
            | display::ItemState::Failed => msg.to_string(),
        },
        UpdateKind::SetOutcomeBuilt
        | UpdateKind::SetOutcomeCached
        | UpdateKind::Cancel
        | UpdateKind::Msg
        | UpdateKind::Step => msg.to_string(),
    }
}

fn plain_repo_message(kind: UpdateKind, state: display::ItemState, msg: &str) -> String {
    match kind {
        UpdateKind::Err => format!("failed: {msg}"),
        UpdateKind::Cached => "cached".to_string(),
        UpdateKind::Fin => match state {
            display::ItemState::Done(_) => "done".to_string(),
            display::ItemState::New
            | display::ItemState::InProgress(_)
            | display::ItemState::Cancelled
            | display::ItemState::Failed => msg.to_string(),
        },
        UpdateKind::SetOutcomeBuilt
        | UpdateKind::SetOutcomeCached
        | UpdateKind::Cancel
        | UpdateKind::Msg
        | UpdateKind::Step => msg.to_string(),
    }
}

fn render_final_report<B: Backend>(
    terminal: &mut ratatui::Terminal<B>,
    lines: Vec<Line<'static>>,
) -> Result<(), B::Error> {
    let viewport_height = current_viewport_height(terminal)?;
    let (prefix, suffix) = split_lines_for_viewport(lines, viewport_height);
    insert_lines_before_viewport(terminal, prefix)?;
    draw_lines_in_viewport(terminal, suffix)
}

fn spacer() -> Span<'static> {
    Span::raw(" ")
}

fn split_lines_for_viewport(
    mut lines: Vec<Line<'static>>,
    viewport_height: u16,
) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let suffix_len = usize::from(viewport_height).min(lines.len());
    let split_at = lines.len() - suffix_len;
    let suffix = lines.split_off(split_at);
    (lines, suffix)
}

fn start_item(state: display::ItemState) -> display::ItemState {
    match state {
        display::ItemState::New => display::ItemState::InProgress(None),
        display::ItemState::InProgress(outcome) => display::ItemState::InProgress(outcome),
        display::ItemState::Done(_)
        | display::ItemState::Cancelled
        | display::ItemState::Failed => state,
    }
}

// ============================================================
// Impls
// ============================================================

impl Addr for DisplayAddr {
    type Message = Message;

    fn name() -> &'static str {
        "DisplayAddr"
    }

    fn sender(&self) -> &mpsc::Sender<Self::Message> {
        &self.tx
    }
}

impl DisplayActor {
    fn aggregate_child_done_outcome(
        &self,
        repo_id: display::ItemId,
    ) -> Option<display::SuccessOutcome> {
        let mut saw_cached = false;

        for grammar in self
            .state
            .grammars
            .values()
            .filter(|g| g.repo_id == Some(repo_id))
        {
            match grammar.state {
                display::ItemState::Done(display::SuccessOutcome::Built) => {
                    return Some(display::SuccessOutcome::Built)
                }
                display::ItemState::Done(display::SuccessOutcome::Cached) => saw_cached = true,
                display::ItemState::New
                | display::ItemState::InProgress(_)
                | display::ItemState::Cancelled
                | display::ItemState::Failed => {}
            }
        }

        saw_cached.then_some(display::SuccessOutcome::Cached)
    }

    fn aggregate_child_live_outcome(
        &self,
        repo_id: display::ItemId,
    ) -> Option<display::SuccessOutcome> {
        let mut saw_cached = false;
        let mut saw_unknown = false;

        for grammar in self
            .state
            .grammars
            .values()
            .filter(|g| g.repo_id == Some(repo_id))
        {
            match grammar.state {
                display::ItemState::InProgress(Some(display::SuccessOutcome::Built))
                | display::ItemState::Done(display::SuccessOutcome::Built) => {
                    return Some(display::SuccessOutcome::Built)
                }
                display::ItemState::InProgress(Some(display::SuccessOutcome::Cached))
                | display::ItemState::Done(display::SuccessOutcome::Cached) => saw_cached = true,
                display::ItemState::New | display::ItemState::InProgress(None) => {
                    saw_unknown = true;
                }
                display::ItemState::Cancelled | display::ItemState::Failed => {}
            }
        }

        if saw_unknown {
            None
        } else if saw_cached {
            Some(display::SuccessOutcome::Cached)
        } else {
            None
        }
    }

    fn apply_grammar_update(grammar: &mut display::GrammarEntry, kind: UpdateKind, msg: Arc<str>) {
        if !grammar.state.is_live() {
            return;
        }

        match kind {
            UpdateKind::Msg => {
                grammar.msg = msg;
            }
            UpdateKind::Step => {
                grammar.state = start_item(grammar.state);
                grammar.step += 1;
                grammar.msg = msg;
            }
            UpdateKind::SetOutcomeCached => {
                grammar.state = mark_success(grammar.state, display::SuccessOutcome::Cached);
            }
            UpdateKind::Cancel => {
                grammar.frozen_elapsed = Some(grammar.started_at.elapsed());
                grammar.state = display::ItemState::Cancelled;
                grammar.step = grammar.total;
                grammar.msg = msg;
            }
            UpdateKind::SetOutcomeBuilt => {
                grammar.state = mark_success(grammar.state, display::SuccessOutcome::Built);
            }
            UpdateKind::Cached => {
                grammar.frozen_elapsed = Some(grammar.started_at.elapsed());
                grammar.state = display::ItemState::Done(display::SuccessOutcome::Cached);
                grammar.step = grammar.total;
                grammar.msg = msg;
            }
            UpdateKind::Fin => {
                grammar.frozen_elapsed = Some(grammar.started_at.elapsed());
                match finish_item(grammar.state) {
                    Ok(state) => {
                        grammar.state = state;
                        grammar.msg = msg;
                    }
                    Err(message) => {
                        grammar.state = display::ItemState::Failed;
                        grammar.msg = message.into();
                    }
                }
                grammar.step = grammar.total;
            }
            UpdateKind::Err => {
                grammar.frozen_elapsed = Some(grammar.started_at.elapsed());
                grammar.state = display::ItemState::Failed;
                grammar.msg = msg;
            }
        }
    }

    fn apply_repo_update(repo: &mut display::RepoEntry, kind: UpdateKind, msg: Arc<str>) {
        if !repo.state.is_live() {
            return;
        }

        match kind {
            UpdateKind::Msg => {
                repo.msg = msg;
            }
            UpdateKind::Step => {
                repo.state = start_item(repo.state);
                repo.step += 1;
                repo.msg = msg;
            }
            UpdateKind::SetOutcomeCached => {
                repo.state = mark_success(repo.state, display::SuccessOutcome::Cached);
            }
            UpdateKind::Cancel => {
                repo.frozen_elapsed = Some(repo.started_at.elapsed());
                repo.state = display::ItemState::Cancelled;
                if repo.total > 0 {
                    repo.step = repo.total;
                }
                repo.msg = msg;
            }
            UpdateKind::SetOutcomeBuilt => {
                repo.state = mark_success(repo.state, display::SuccessOutcome::Built);
            }
            UpdateKind::Cached => {
                repo.frozen_elapsed = Some(repo.started_at.elapsed());
                repo.state = display::ItemState::Done(display::SuccessOutcome::Cached);
                if repo.total > 0 {
                    repo.step = repo.total;
                }
                repo.msg = msg;
            }
            UpdateKind::Fin => {
                repo.frozen_elapsed = Some(repo.started_at.elapsed());
                match finish_item(repo.state) {
                    Ok(state) => {
                        repo.state = state;
                        repo.msg = msg;
                    }
                    Err(message) => {
                        repo.state = display::ItemState::Failed;
                        repo.msg = message.into();
                    }
                }
                if repo.total > 0 {
                    repo.step = repo.total;
                }
            }
            UpdateKind::Err => {
                repo.frozen_elapsed = Some(repo.started_at.elapsed());
                repo.state = display::ItemState::Failed;
                repo.msg = msg;
            }
        }
    }

    fn apply_update(&mut self, id: display::ItemId, kind: UpdateKind, msg: Arc<str>) {
        if let Some(repo) = self.state.repos.get_mut(&id) {
            Self::apply_repo_update(repo, kind, msg);
            self.grid.mark_dirty(id);
            return;
        }

        let mut maybe_parent_id: Option<display::ItemId> = None;
        if let Some(grammar) = self.state.grammars.get_mut(&id) {
            Self::apply_grammar_update(grammar, kind, msg);
            self.grid.mark_dirty(id);
            if matches!(
                kind,
                UpdateKind::Cached
                    | UpdateKind::Err
                    | UpdateKind::Fin
                    | UpdateKind::SetOutcomeBuilt
                    | UpdateKind::SetOutcomeCached
                    | UpdateKind::Cancel
                    | UpdateKind::Step
            ) {
                maybe_parent_id = grammar.repo_id;
            }
        }

        if let Some(repo_id) = maybe_parent_id {
            self.sync_parent_repo(repo_id);
        }
    }

    fn cancel_live_rows(&mut self) {
        let mut parent_ids = Vec::new();

        for (id, grammar) in &mut self.state.grammars {
            if grammar.state.is_live() {
                grammar.frozen_elapsed = Some(grammar.started_at.elapsed());
                grammar.state = display::ItemState::Cancelled;
                grammar.step = grammar.total;
                grammar.msg = Arc::from("cancelled");
                if let Some(repo_id) = grammar.repo_id {
                    parent_ids.push(repo_id);
                }
                self.grid.mark_dirty(*id);
            }
        }

        parent_ids.sort_unstable();
        parent_ids.dedup();
        for repo_id in parent_ids {
            self.sync_parent_repo(repo_id);
        }

        for (id, repo) in &mut self.state.repos {
            if repo.state.is_live() {
                repo.frozen_elapsed = Some(repo.started_at.elapsed());
                repo.state = display::ItemState::Cancelled;
                if repo.total > 0 {
                    repo.step = repo.total;
                }
                repo.msg = Arc::from("cancelled");
                self.grid.mark_dirty(*id);
            }
        }
    }

    fn finish_fancy<B: Backend<Error = io::Error>>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
        interrupted: bool,
        tx: oneshot::Sender<()>,
    ) {
        if interrupted {
            self.cancel_live_rows();
        }
        let term_width = terminal.size().map_or(80, |s| s.width);
        let mut final_lines = self.materialize(term_width);
        final_lines.push(Line::from(""));

        match render_final_report(terminal, final_lines) {
            Ok(()) => {
                ratatui::restore();
                if let Err(err) = clear_from_cursor_down() {
                    println!();
                    eprintln!("tsdl: fancy display cleanup failed: {err}");
                }
            }
            Err(err) => {
                ratatui::restore();
                println!();
                eprintln!("tsdl: fancy display final report failed: {err}");
            }
        }

        let _ = tx.send(());
    }

    fn handle_message(&mut self, msg: Message) {
        match msg {
            Message::RegisterLanguage {
                git_ref,
                name,
                num_tasks,
                tx,
            } => {
                let addr = self.register_repo(name, git_ref, num_tasks);
                let _ = tx.send(addr);
            }
            Message::RegisterGrammar {
                git_ref,
                language,
                name,
                num_tasks,
                tx,
            } => {
                let addr = self.register_grammar(language, name, git_ref, num_tasks);
                let _ = tx.send(addr);
            }
            Message::RegisterReference { .. } => {}
            Message::Update { id, kind, msg } => {
                self.apply_update(id, kind, msg);
            }
            Message::Shutdown { interrupted, tx } => {
                if interrupted {
                    self.cancel_live_rows();
                }
                let _ = tx.send(());
            }
        }
    }

    /// Assemble `Vec<Line>` from the grid cache. Rebuilds row order and layout
    /// only when items have been added. Otherwise reuses the cached row order
    /// and recomputes only stale cells (dirty items + running clocks).
    fn materialize(&mut self, term_width: u16) -> Vec<Line<'static>> {
        let term_w = term_width as usize;

        if self.last_term_width != Some(term_w) {
            self.grid.invalidate_column(display::Column::Msg);
            self.last_term_width = Some(term_w);
        }

        // Rebuild row order if items were added
        if self.rows_dirty {
            self.row_specs = self.state.compute_row_order();
            let new_layout = self.state.compute_layout(&self.row_specs);

            // Invalidate columns whose width changed
            if new_layout.ref_ != self.grid.layout.ref_ {
                self.grid.invalidate_column(display::Column::Ref);
            }
            if new_layout.step != self.grid.layout.step {
                self.grid.invalidate_column(display::Column::Step);
            }
            if new_layout.name != self.grid.layout.name {
                self.grid.invalidate_column(display::Column::Name);
            }
            if new_layout.fixed_width != self.grid.layout.fixed_width {
                self.grid.invalidate_column(display::Column::Msg);
            }
            self.grid.layout = new_layout;
            self.rows_dirty = false;
        }

        // Assemble rows from the grid cache
        let layout = self.grid.layout.clone();
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(self.row_specs.len());

        for spec in &self.row_specs {
            let info = self.state.get_item_info(spec);
            let item_id = spec.id;
            let is_dirty = self.grid.dirty_items.contains(&item_id);

            // TIME: stale when dirty OR clock is still running
            let time_stale = is_dirty || info.state().is_live();
            let time = self
                .grid
                .cell(item_id, display::Column::Time, time_stale, || {
                    display::compute_time_cell(&info, &layout)
                });

            let gref = self.grid.cell(item_id, display::Column::Ref, is_dirty, || {
                display::compute_ref_cell(&info, &layout)
            });

            let step = self
                .grid
                .cell(item_id, display::Column::Step, is_dirty, || {
                    display::compute_step_cell(&info, &layout)
                });

            let icon = self
                .grid
                .cell(item_id, display::Column::Icon, is_dirty, || {
                    display::compute_icon_cell(&info)
                });

            let name = self
                .grid
                .cell(item_id, display::Column::Name, is_dirty, || {
                    display::compute_name_cell(&spec.display_name, spec.indent, &info, &layout)
                });

            let msg = self.grid.cell(item_id, display::Column::Msg, is_dirty, || {
                display::compute_msg_cell(&info, &layout, term_w)
            });

            lines.push(Line::from(vec![
                time,
                spacer(),
                gref,
                spacer(),
                step,
                spacer(),
                icon,
                spacer(),
                name,
                spacer(),
                msg,
            ]));
        }

        // Footer
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(self.state.footer_build_line());
        lines.push(self.state.footer_out_line());
        lines.push(Line::from(""));
        lines.push(self.state.format_footer_counts());

        self.grid.clear_dirty();

        lines
    }

    fn plain_progress_line(&self, id: display::ItemId, kind: UpdateKind) -> Option<PlainLine> {
        if let Some(repo) = self.state.repos.get(&id) {
            return Some(PlainLine {
                name: repo.name.to_string(),
                step: repo.step,
                total: repo.total,
                message: plain_repo_message(kind, repo.state, &repo.msg),
            });
        }

        self.state.grammars.get(&id).map(|grammar| PlainLine {
            name: format!("{}/{}", grammar.repo, grammar.name),
            step: grammar.step,
            total: grammar.total,
            message: plain_grammar_message(kind, grammar.state, &grammar.msg),
        })
    }

    fn print_plain_metadata(&self) {
        println!("build: {}", self.state.build_dir.display());
        println!("out:   {}", self.state.out_dir.display());
        println!();
    }

    fn print_plain_progress(&mut self, line: &PlainLine) {
        if !self.plain_progress_started {
            println!();
            self.plain_progress_started = true;
        }

        let width = self.update_plain_name_width(&line.name);
        println!(
            "{name:<width$} [{step}/{total}] {message}",
            name = line.name,
            step = line.step.min(line.total),
            total = line.total,
            message = line.message,
        );
    }

    fn print_plain_ref(&mut self, name: &str, git_ref: &str) {
        let width = self.update_plain_name_width(name);
        println!("{name:<width$} @ {git_ref}");
    }

    fn print_plain_summary(&self) {
        let (cached, built, _building, failed, cancelled) = self.state.summary_counts();
        println!();
        println!("✓ {cached} cached  ✓ {built} built  ✗ {cancelled} cancelled  ✗ {failed} failed");
    }

    fn register_grammar(
        &mut self,
        language: Arc<str>,
        name: Arc<str>,
        git_ref: git::Ref,
        num_tasks: usize,
    ) -> ProgressAddr {
        let id = self.next_id;
        self.next_id = self.next_id.next_after();

        let repo_id = self
            .state
            .repos
            .iter()
            .find(|(_, r)| r.name == language)
            .map(|(id, _)| *id);

        self.state.grammars.insert(
            id,
            display::GrammarEntry {
                repo: language,
                repo_id,
                name,
                git_ref,
                state: display::ItemState::New,
                msg: Arc::from(""),
                step: 0,
                total: num_tasks,
                started_at: Instant::now(),
                frozen_elapsed: None,
            },
        );

        self.rows_dirty = true;
        if let Some(repo_id) = repo_id {
            self.sync_parent_repo(repo_id);
        }

        ProgressAddr {
            id,
            tx: self.tx.clone(),
        }
    }

    fn register_repo(
        &mut self,
        name: Arc<str>,
        git_ref: git::Ref,
        num_tasks: usize,
    ) -> ProgressAddr {
        let id = self.next_id;
        self.next_id = self.next_id.next_after();

        self.state.repos.insert(
            id,
            display::RepoEntry {
                name,
                git_ref,
                state: display::ItemState::New,
                msg: Arc::from(""),
                step: 0,
                total: num_tasks,
                started_at: Instant::now(),
                frozen_elapsed: None,
            },
        );

        self.rows_dirty = true;

        ProgressAddr {
            id,
            tx: self.tx.clone(),
        }
    }

    async fn run(mut self) {
        if self.state.mode == display::Mode::Fancy {
            self.run_fancy().await;
        } else {
            self.run_plain().await;
        }
    }

    // ── Fancy mode ────────────────────────────────────────────────────

    async fn run_fancy(&mut self) {
        let viewport_height = crossterm::terminal::size().map_or(40, |(_, h)| h).min(40);

        let mut terminal = match ratatui::Terminal::with_options(
            ratatui::backend::CrosstermBackend::new(io::stdout()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Inline(viewport_height),
            },
        ) {
            Ok(terminal) => terminal,
            Err(err) => {
                eprintln!("tsdl: fancy display unavailable; falling back to plain progress: {err}");
                self.run_plain().await;
                return;
            }
        };

        // One-time bootstrap render so the user sees state immediately.
        let lines = self.materialize(terminal.size().map_or(80, |s| s.width));
        if let Err(err) = draw_lines(&mut terminal, lines) {
            ratatui::restore();
            println!();
            eprintln!("tsdl: fancy display failed; falling back to plain progress: {err}");
            self.run_plain().await;
            return;
        }

        let mut tick_interval = time::interval(Duration::from_millis(100));

        loop {
            // ── wait for the next event ─────────────────────────────
            let mut shutdown = None;
            tokio::select! {
                msg = self.rx.recv() => {
                    match msg {
                        Some(Message::Shutdown { interrupted, tx }) => {
                            shutdown = Some((interrupted, tx));
                        }
                        Some(other) => self.handle_message(other),
                        None => break,
                    }
                }
                _ = tick_interval.tick() => {
                    // fall through to drain + render
                }
            }

            // ── drain any messages that queued up concurrently ─────
            while let Ok(msg) = self.rx.try_recv() {
                match msg {
                    Message::Shutdown { interrupted, tx } => {
                        shutdown = Some((interrupted, tx));
                        break;
                    }
                    other => self.handle_message(other),
                }
            }

            // ── shutdown after draining ────────────────────────────
            if let Some((interrupted, tx)) = shutdown {
                self.finish_fancy(&mut terminal, interrupted, tx);
                return;
            }

            // ── render once per event cycle ────────────────────────
            let lines = self.materialize(terminal.size().map_or(80, |s| s.width));
            if let Err(err) = draw_lines(&mut terminal, lines) {
                ratatui::restore();
                println!();
                eprintln!("tsdl: fancy display failed; falling back to plain progress: {err}");
                self.run_plain().await;
                return;
            }
        }

        // #[allow(unreachable_code)]
        // ratatui::restore();
        // println!();
    }

    // ── Plain mode ────────────────────────────────────────────────────

    async fn run_plain(&mut self) {
        self.print_plain_metadata();

        while let Some(msg) = self.rx.recv().await {
            match msg {
                Message::RegisterLanguage {
                    git_ref,
                    name,
                    num_tasks,
                    tx,
                } => {
                    let addr = self.register_repo(name, git_ref, num_tasks);
                    let _ = tx.send(addr);
                }
                Message::RegisterGrammar {
                    git_ref,
                    language,
                    name,
                    num_tasks,
                    tx,
                } => {
                    let plain_name = format!("{language}/{name}");
                    self.update_plain_name_width(&plain_name);
                    let addr = self.register_grammar(language, name, git_ref, num_tasks);
                    let _ = tx.send(addr);
                }
                Message::RegisterReference { git_ref, name } => {
                    self.print_plain_ref(&name, git_ref.short());
                }
                Message::Update { id, kind, msg } => {
                    self.apply_update(id, kind, msg);
                    if matches!(
                        kind,
                        UpdateKind::Msg
                            | UpdateKind::SetOutcomeCached
                            | UpdateKind::SetOutcomeBuilt
                    ) {
                        continue;
                    }
                    if let Some(line) = self.plain_progress_line(id, kind) {
                        self.print_plain_progress(&line);
                    }
                }
                Message::Shutdown { interrupted, tx } => {
                    if interrupted {
                        self.cancel_live_rows();
                    }
                    self.print_plain_summary();
                    let _ = tx.send(());
                    break;
                }
            }
        }
    }

    #[must_use]
    pub fn spawn(mode: display::Mode, build_dir: PathBuf, out_dir: PathBuf) -> DisplayAddr {
        let (tx, rx) = mpsc::channel(256);
        let actor = Self {
            state: display::State::new(mode, build_dir, out_dir),
            next_id: display::ItemId::new(NonZeroU64::MIN),
            plain_name_width: 16,
            plain_progress_started: false,
            grid: display::GridCache::new(),
            row_specs: Vec::new(),
            rows_dirty: true,
            last_term_width: None,
            rx,
            tx: tx.clone(),
        };

        tokio::spawn(async move {
            actor.run().await;
        });

        DisplayAddr::new(tx, mode)
    }

    fn sync_parent_repo(&mut self, repo_id: display::ItemId) {
        let has_any = self
            .state
            .grammars
            .values()
            .any(|g| g.repo_id == Some(repo_id));
        if !has_any {
            return;
        }

        let any_failed = self
            .state
            .grammars
            .values()
            .any(|g| g.repo_id == Some(repo_id) && g.state == display::ItemState::Failed);
        let any_cancelled = self
            .state
            .grammars
            .values()
            .any(|g| g.repo_id == Some(repo_id) && g.state == display::ItemState::Cancelled);
        let any_active = self
            .state
            .grammars
            .values()
            .any(|g| g.repo_id == Some(repo_id) && g.state.is_live());

        let live_outcome = self.aggregate_child_live_outcome(repo_id);
        let done_outcome = self.aggregate_child_done_outcome(repo_id);

        if let Some(repo) = self.state.repos.get_mut(&repo_id) {
            if any_active {
                repo.state = display::ItemState::InProgress(live_outcome);
                repo.msg = Arc::from("building");
                repo.frozen_elapsed = None;
            } else if any_failed {
                repo.state = display::ItemState::Failed;
                repo.msg = Arc::from("failed");
                repo.frozen_elapsed
                    .get_or_insert_with(|| repo.started_at.elapsed());
            } else if any_cancelled {
                repo.state = display::ItemState::Cancelled;
                repo.msg = Arc::from("cancelled");
                repo.frozen_elapsed
                    .get_or_insert_with(|| repo.started_at.elapsed());
            } else {
                repo.state = display::ItemState::Done(
                    done_outcome.unwrap_or(display::SuccessOutcome::Built),
                );
                repo.msg = Arc::from("done");
                repo.frozen_elapsed
                    .get_or_insert_with(|| repo.started_at.elapsed());
            }
        }

        self.grid.mark_dirty(repo_id);
    }

    fn update_plain_name_width(&mut self, name: &str) -> usize {
        self.plain_name_width = self.plain_name_width.max(name.chars().count());
        self.plain_name_width
    }
}

impl DisplayAddr {
    pub async fn add_language<S: Into<Arc<str>>>(
        &self,
        git_ref: git::Ref,
        name: S,
        num_tasks: usize,
    ) -> ProgressAddr {
        self.request(|tx| Message::RegisterLanguage {
            git_ref,
            name: name.into(),
            num_tasks,
            tx,
        })
        .await
    }

    pub async fn add_grammar<S: Into<Arc<str>>>(
        &self,
        git_ref: git::Ref,
        language: S,
        name: S,
        num_tasks: usize,
    ) -> ProgressAddr {
        self.request(|tx| Message::RegisterGrammar {
            git_ref,
            language: language.into(),
            name: name.into(),
            num_tasks,
            tx,
        })
        .await
    }

    #[must_use]
    pub fn new(tx: mpsc::Sender<Message>, mode: display::Mode) -> Self {
        Self { tx, mode }
    }

    pub async fn reference<S: Into<Arc<str>>>(&self, git_ref: git::Ref, name: S) {
        self.fire(Message::RegisterReference {
            git_ref,
            name: name.into(),
        })
        .await;
    }

    pub async fn shutdown(&self, interrupted: bool) {
        self.request(|tx| Message::Shutdown { interrupted, tx })
            .await;
    }
}

impl ProgressAddr {
    pub fn msg<S: Into<Arc<str>>>(&self, msg: S) {
        let _ = self.tx.try_send(Message::Update {
            id: self.id,
            kind: UpdateKind::Msg,
            msg: msg.into(),
        });
    }

    pub fn step<S: Into<Arc<str>>>(&self, msg: S) {
        let _ = self.tx.try_send(Message::Update {
            id: self.id,
            kind: UpdateKind::Step,
            msg: msg.into(),
        });
    }

    async fn send_state_update(&self, kind: UpdateKind, msg: Arc<str>) {
        let _ = self
            .tx
            .send(Message::Update {
                id: self.id,
                kind,
                msg,
            })
            .await;
    }

    pub async fn set_outcome_cached(&self) {
        self.send_state_update(UpdateKind::SetOutcomeCached, Arc::from(""))
            .await;
    }

    pub async fn cancel(&self) {
        self.send_state_update(UpdateKind::Cancel, Arc::from("cancelled"))
            .await;
    }

    pub async fn set_outcome_built(&self) {
        self.send_state_update(UpdateKind::SetOutcomeBuilt, Arc::from(""))
            .await;
    }

    pub async fn cached<S: Into<Arc<str>>>(&self, msg: S) {
        self.send_state_update(UpdateKind::Cached, msg.into()).await;
    }

    pub async fn fin<S: Into<Arc<str>>>(&self, msg: S) {
        self.send_state_update(UpdateKind::Fin, msg.into()).await;
    }

    pub async fn err<S: Into<Arc<str>>>(&self, msg: S) {
        self.send_state_update(UpdateKind::Err, msg.into()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> DisplayActor {
        let (tx, rx) = mpsc::channel(1);
        DisplayActor {
            state: display::State::new(
                display::Mode::Fancy,
                PathBuf::from("build"),
                PathBuf::from("out"),
            ),
            next_id: display::ItemId::new(NonZeroU64::MIN),
            plain_name_width: 16,
            plain_progress_started: false,
            grid: display::GridCache::new(),
            row_specs: Vec::new(),
            rows_dirty: true,
            last_term_width: None,
            rx,
            tx,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn split_lines_for_viewport_keeps_the_visible_suffix() {
        let lines = vec![
            Line::from("one"),
            Line::from("two"),
            Line::from("three"),
            Line::from("four"),
        ];

        let (prefix, suffix) = split_lines_for_viewport(lines, 2);

        assert_eq!(line_text(&prefix[0]), "one");
        assert_eq!(line_text(&prefix[1]), "two");
        assert_eq!(line_text(&suffix[0]), "three");
        assert_eq!(line_text(&suffix[1]), "four");
    }

    #[test]
    fn render_final_report_inserts_prefix_and_replaces_viewport() {
        use ratatui::backend::TestBackend;
        use ratatui::style::{Color, Style};
        use ratatui::{Terminal, TerminalOptions, Viewport};

        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(2),
            },
        )
        .unwrap();

        draw_lines(
            &mut terminal,
            vec![Line::from("old viewport 1"), Line::from("old viewport 2")],
        )
        .unwrap();

        render_final_report(
            &mut terminal,
            vec![
                Line::from("final 1"),
                Line::from("final 2"),
                Line::from("final 3"),
                Line::from(Span::styled("final 4", Style::default().fg(Color::Blue))),
                Line::from(""),
            ],
        )
        .unwrap();

        terminal.backend().assert_buffer_lines(vec![
            Line::from("final 1             "),
            Line::from("final 2             "),
            Line::from("final 3             "),
            Line::from(vec![
                Span::styled("final 4", Style::default().fg(Color::Blue)),
                Span::raw("             "),
            ]),
            Line::from("                    "),
        ]);
        terminal.backend().assert_scrollback_empty();
        terminal.backend_mut().assert_cursor_position((0, 4));
    }

    #[test]
    fn summary_counts_include_repo_only_rows() {
        let mut actor = actor();

        let cached = actor.register_repo("tree-sitter-cli".into(), git::Ref::head(), 2);
        actor.apply_update(cached.id, UpdateKind::SetOutcomeCached, Arc::from(""));
        actor.apply_update(cached.id, UpdateKind::Fin, Arc::from("done"));

        let built = actor.register_repo("standalone".into(), git::Ref::head(), 1);
        actor.apply_update(built.id, UpdateKind::SetOutcomeBuilt, Arc::from(""));
        actor.apply_update(built.id, UpdateKind::Fin, Arc::from("done"));

        let active = actor.register_repo("active".into(), git::Ref::head(), 1);
        actor.apply_update(active.id, UpdateKind::Step, Arc::from("working"));

        let failed = actor.register_repo("failed".into(), git::Ref::head(), 1);
        actor.apply_update(failed.id, UpdateKind::Err, Arc::from("failed"));

        assert_eq!(actor.state.summary_counts(), (1, 1, 1, 1, 0));
    }

    #[test]
    fn summary_counts_do_not_double_count_parent_repos_with_grammars() {
        let mut actor = actor();

        let repo = actor.register_repo("json".into(), git::Ref::head(), 2);
        actor.apply_update(repo.id, UpdateKind::SetOutcomeBuilt, Arc::from(""));
        actor.apply_update(repo.id, UpdateKind::Fin, Arc::from("done"));

        let grammar = actor.register_grammar("json".into(), "json".into(), git::Ref::head(), 4);
        actor.apply_update(grammar.id, UpdateKind::SetOutcomeCached, Arc::from(""));
        actor.apply_update(grammar.id, UpdateKind::Cached, Arc::from("done"));

        assert_eq!(actor.state.summary_counts(), (1, 0, 0, 0, 0));
    }

    #[test]
    fn cancelled_grammar_is_terminal_and_updates_parent_repo() {
        let mut actor = actor();

        let repo = actor.register_repo("json".into(), git::Ref::head(), 2);
        let grammar = actor.register_grammar("json".into(), "json".into(), git::Ref::head(), 4);

        actor.apply_update(grammar.id, UpdateKind::Step, Arc::from("building"));
        actor.apply_update(grammar.id, UpdateKind::Cancel, Arc::from("cancelled"));

        let grammar_entry = actor.state.grammars.get(&grammar.id).unwrap();
        assert_eq!(grammar_entry.state, display::ItemState::Cancelled);
        assert!(!grammar_entry.state.is_live());
        assert_eq!(grammar_entry.step, grammar_entry.total);
        assert!(grammar_entry.frozen_elapsed.is_some());

        let repo_entry = actor.state.repos.get(&repo.id).unwrap();
        assert_eq!(repo_entry.state, display::ItemState::Cancelled);
        assert!(!repo_entry.state.is_live());
        assert_eq!(repo_entry.msg.as_ref(), "cancelled");
        assert!(repo_entry.frozen_elapsed.is_some());

        assert_eq!(actor.state.summary_counts(), (0, 0, 0, 0, 1));
    }

    #[test]
    fn cancelled_child_does_not_cancel_parent_while_sibling_is_active() {
        let mut actor = actor();

        let repo = actor.register_repo("typescript".into(), git::Ref::head(), 2);
        let cancelled = actor.register_grammar(
            "typescript".into(),
            "typescript".into(),
            git::Ref::head(),
            4,
        );
        let active = actor.register_grammar("typescript".into(), "tsx".into(), git::Ref::head(), 4);

        actor.apply_update(cancelled.id, UpdateKind::Step, Arc::from("building"));
        actor.apply_update(cancelled.id, UpdateKind::Cancel, Arc::from("cancelled"));
        actor.apply_update(active.id, UpdateKind::Step, Arc::from("building"));

        let repo_entry = actor.state.repos.get(&repo.id).unwrap();
        assert_eq!(repo_entry.state, display::ItemState::InProgress(None));
        assert_eq!(repo_entry.msg.as_ref(), "building");
        assert!(repo_entry.frozen_elapsed.is_none());

        actor.apply_update(active.id, UpdateKind::Cancel, Arc::from("cancelled"));

        let repo_entry = actor.state.repos.get(&repo.id).unwrap();
        assert_eq!(repo_entry.state, display::ItemState::Cancelled);
        assert_eq!(repo_entry.msg.as_ref(), "cancelled");
        assert!(repo_entry.frozen_elapsed.is_some());
    }

    #[test]
    fn failed_child_wins_over_cancelled_child_when_parent_terminal() {
        let mut actor = actor();

        let repo = actor.register_repo("typescript".into(), git::Ref::head(), 2);
        let failing = actor.register_grammar(
            "typescript".into(),
            "typescript".into(),
            git::Ref::head(),
            4,
        );
        let cancelled =
            actor.register_grammar("typescript".into(), "tsx".into(), git::Ref::head(), 4);

        actor.apply_update(failing.id, UpdateKind::Step, Arc::from("building"));
        actor.apply_update(failing.id, UpdateKind::Err, Arc::from("build failed"));
        actor.apply_update(cancelled.id, UpdateKind::Step, Arc::from("building"));
        actor.apply_update(cancelled.id, UpdateKind::Cancel, Arc::from("cancelled"));

        let repo_entry = actor.state.repos.get(&repo.id).unwrap();
        assert_eq!(repo_entry.state, display::ItemState::Failed);
        assert_eq!(repo_entry.msg.as_ref(), "failed");
        assert!(repo_entry.frozen_elapsed.is_some());

        assert_eq!(actor.state.summary_counts(), (0, 0, 0, 1, 1));
    }

    #[test]
    #[should_panic(expected = "finish update received before cached/built path was set")]
    fn finish_without_cached_or_built_path_panics() {
        let mut actor = actor();
        let repo = actor.register_repo("json".into(), git::Ref::head(), 2);

        actor.apply_update(repo.id, UpdateKind::Step, Arc::from("scanning"));
        actor.apply_update(repo.id, UpdateKind::Fin, Arc::from("done"));
    }

    #[test]
    fn interrupted_shutdown_cancels_live_rows() {
        let mut actor = actor();

        let repo = actor.register_repo("typescript".into(), git::Ref::head(), 2);
        let grammar =
            actor.register_grammar("typescript".into(), "tsx".into(), git::Ref::head(), 4);
        actor.apply_update(grammar.id, UpdateKind::Step, Arc::from("building"));

        actor.cancel_live_rows();

        let grammar_entry = actor.state.grammars.get(&grammar.id).unwrap();
        assert_eq!(grammar_entry.state, display::ItemState::Cancelled);
        assert_eq!(grammar_entry.step, grammar_entry.total);

        let repo_entry = actor.state.repos.get(&repo.id).unwrap();
        assert_eq!(repo_entry.state, display::ItemState::Cancelled);
        assert_eq!(repo_entry.msg.as_ref(), "cancelled");

        assert_eq!(actor.state.summary_counts(), (0, 0, 0, 0, 1));
    }

    #[test]
    fn terminal_direct_updates_are_absorbing() {
        let mut actor = actor();

        let repo = actor.register_repo("json".into(), git::Ref::head(), 4);
        actor.apply_update(repo.id, UpdateKind::Step, Arc::from("building"));
        actor.apply_update(repo.id, UpdateKind::Cancel, Arc::from("cancelled"));
        actor.apply_update(repo.id, UpdateKind::Cached, Arc::from("done"));
        actor.apply_update(repo.id, UpdateKind::Err, Arc::from("failed"));

        let entry = actor.state.repos.get(&repo.id).unwrap();
        assert_eq!(entry.state, display::ItemState::Cancelled);
        assert_eq!(entry.msg.as_ref(), "cancelled");

        let grammar = actor.register_grammar("json".into(), "json".into(), git::Ref::head(), 4);
        actor.apply_update(grammar.id, UpdateKind::Step, Arc::from("building"));
        actor.apply_update(grammar.id, UpdateKind::Cancel, Arc::from("cancelled"));
        actor.apply_update(grammar.id, UpdateKind::Cached, Arc::from("done"));
        actor.apply_update(grammar.id, UpdateKind::Err, Arc::from("failed"));

        let entry = actor.state.grammars.get(&grammar.id).unwrap();
        assert_eq!(entry.state, display::ItemState::Cancelled);
        assert_eq!(entry.msg.as_ref(), "cancelled");
    }

    #[test]
    fn parent_repo_stays_active_while_any_child_is_active() {
        let mut actor = actor();

        let repo = actor.register_repo("typescript".into(), git::Ref::head(), 2);
        let failing = actor.register_grammar(
            "typescript".into(),
            "typescript".into(),
            git::Ref::head(),
            4,
        );
        let pending =
            actor.register_grammar("typescript".into(), "tsx".into(), git::Ref::head(), 4);

        actor.apply_update(failing.id, UpdateKind::Step, Arc::from("building"));
        actor.apply_update(failing.id, UpdateKind::Err, Arc::from("build failed"));

        let repo_entry = actor.state.repos.get(&repo.id).unwrap();
        assert_eq!(repo_entry.state, display::ItemState::InProgress(None));
        assert_eq!(repo_entry.msg.as_ref(), "building");
        assert!(repo_entry.frozen_elapsed.is_none());

        actor.apply_update(pending.id, UpdateKind::SetOutcomeBuilt, Arc::from(""));
        actor.apply_update(pending.id, UpdateKind::Fin, Arc::from("done"));

        let repo_entry = actor.state.repos.get(&repo.id).unwrap();
        assert_eq!(repo_entry.state, display::ItemState::Failed);
        assert_eq!(repo_entry.msg.as_ref(), "failed");
        assert!(repo_entry.frozen_elapsed.is_some());
    }

    #[test]
    fn materialize_recomputes_message_cells_when_terminal_width_changes() {
        let mut actor = actor();
        let progress = actor.register_repo("json".into(), git::Ref::head(), 1);
        let message = "abcdefghijklmnop";

        actor.apply_update(progress.id, UpdateKind::Msg, Arc::from(message));

        let narrow_lines = actor.materialize(30);
        let narrow_row = line_text(&narrow_lines[0]);
        assert!(narrow_row.contains("abcd…"));
        assert!(!narrow_row.contains(message));

        let wide_lines = actor.materialize(80);
        let wide_row = line_text(&wide_lines[0]);
        assert!(wide_row.contains(message));
    }

    #[test]
    fn materialize_recomputes_message_cells_when_layout_width_changes() {
        let mut actor = actor();
        let progress = actor.register_repo("json".into(), git::Ref::head(), 1);
        let message = "abcdefghijklmnop";

        actor.apply_update(progress.id, UpdateKind::Msg, Arc::from(message));

        let initial_lines = actor.materialize(42);
        let initial_row = line_text(&initial_lines[0]);
        assert!(initial_row.contains(message));

        actor.register_repo("zzzzzzzzzzzzzzzzzzzz".into(), git::Ref::head(), 1);

        let updated_lines = actor.materialize(42);
        let updated_row = updated_lines
            .iter()
            .map(line_text)
            .find(|line| line.contains("json"))
            .expect("json row should still be rendered");
        assert!(updated_row.contains('…'));
        assert!(!updated_row.contains(message));
    }
}
