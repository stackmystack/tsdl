use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::backend::Backend;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};
use tokio::sync::{mpsc, oneshot};
use tokio::time;

use crate::actors::Addr;
use crate::display::{
    compute_icon_cell, compute_msg_cell, compute_name_cell, compute_ref_cell, compute_step_cell,
    compute_time_cell, Column, DisplayState, GrammarEntry, GridCache, ItemOutcome, ItemStatus,
    Mode, RepoEntry, RowSpec,
};
use crate::git::GitRef;

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum DisplayMessage {
    /// Register a repo-level progress line. Returns a `ProgressAddr`.
    RegisterLanguage {
        git_ref: GitRef,
        name: Arc<str>,
        num_tasks: usize,
        tx: oneshot::Sender<ProgressAddr>,
    },

    /// Register a grammar-level progress line. Returns a `ProgressAddr`.
    RegisterGrammar {
        git_ref: GitRef,
        language: Arc<str>,
        name: Arc<str>,
        num_tasks: usize,
        tx: oneshot::Sender<ProgressAddr>,
    },

    /// Plain-mode reference line. Fancy mode ignores this because refs are rendered per row.
    RegisterReference { git_ref: GitRef, name: Arc<str> },

    /// Update a specific bar.
    Update {
        id: u64,
        kind: UpdateKind,
        msg: Arc<str>,
    },

    /// Flush and close the display actor. The response is sent after cleanup.
    Shutdown { tx: oneshot::Sender<()> },
}

#[derive(Debug, Clone, Copy)]
pub enum UpdateKind {
    Msg,
    Step,
    MarkCached,
    MarkBuilt,
    MarkCancelled,
    Cached,
    Fin,
    Err,
}

// ---------------------------------------------------------------------------
// Handles
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DisplayAddr {
    tx: mpsc::Sender<DisplayMessage>,
    #[allow(dead_code)]
    mode: Mode,
}

impl Addr for DisplayAddr {
    type Message = DisplayMessage;

    fn name() -> &'static str {
        "DisplayAddr"
    }

    fn sender(&self) -> &mpsc::Sender<Self::Message> {
        &self.tx
    }
}

impl DisplayAddr {
    #[must_use]
    pub fn new(tx: mpsc::Sender<DisplayMessage>, mode: Mode) -> Self {
        Self { tx, mode }
    }

    pub async fn add_language<S: Into<Arc<str>>>(
        &self,
        git_ref: GitRef,
        name: S,
        num_tasks: usize,
    ) -> ProgressAddr {
        self.request(|tx| DisplayMessage::RegisterLanguage {
            git_ref,
            name: name.into(),
            num_tasks,
            tx,
        })
        .await
    }

    pub async fn add_grammar<S: Into<Arc<str>>>(
        &self,
        git_ref: GitRef,
        language: S,
        name: S,
        num_tasks: usize,
    ) -> ProgressAddr {
        self.request(|tx| DisplayMessage::RegisterGrammar {
            git_ref,
            language: language.into(),
            name: name.into(),
            num_tasks,
            tx,
        })
        .await
    }

    pub async fn reference<S: Into<Arc<str>>>(&self, git_ref: GitRef, name: S) {
        self.fire(DisplayMessage::RegisterReference {
            git_ref,
            name: name.into(),
        })
        .await;
    }

    pub async fn shutdown(&self) {
        self.request(|tx| DisplayMessage::Shutdown { tx }).await;
    }
}

/// Handle for updating a specific progress bar (repo or grammar).
#[derive(Debug, Clone)]
pub struct ProgressAddr {
    id: u64,
    tx: mpsc::Sender<DisplayMessage>,
}

impl ProgressAddr {
    pub fn msg<S: Into<Arc<str>>>(&self, msg: S) {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Msg,
            msg: msg.into(),
        });
    }

    pub fn step<S: Into<Arc<str>>>(&self, msg: S) {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Step,
            msg: msg.into(),
        });
    }

    pub fn mark_cached(&self) {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::MarkCached,
            msg: Arc::from(""),
        });
    }

    pub fn mark_cancelled(&self) {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::MarkCancelled,
            msg: Arc::from("cancelled"),
        });
    }

    pub fn mark_built(&self) {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::MarkBuilt,

            msg: Arc::from(""),
        });
    }

    pub fn cached<S: Into<Arc<str>>>(&self, msg: S) {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Cached,
            msg: msg.into(),
        });
    }

    pub fn fin<S: Into<Arc<str>>>(&self, msg: S) {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Fin,
            msg: msg.into(),
        });
    }

    pub fn err<S: Into<Arc<str>>>(&self, msg: S) {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Err,
            msg: msg.into(),
        });
    }
}

// ---------------------------------------------------------------------------
// Plain output model
// ---------------------------------------------------------------------------

struct PlainLine {
    name: String,
    step: usize,
    total: usize,
    message: String,
}

fn plain_repo_message(kind: UpdateKind, outcome: ItemOutcome, msg: &str) -> String {
    match kind {
        UpdateKind::Err => format!("failed: {msg}"),
        UpdateKind::Cached => "cached".to_string(),
        UpdateKind::Fin => match outcome {
            ItemOutcome::Built | ItemOutcome::Cached => "done".to_string(),
            ItemOutcome::Unknown | ItemOutcome::Cancelled => msg.to_string(),
        },
        UpdateKind::MarkBuilt
        | UpdateKind::MarkCached
        | UpdateKind::MarkCancelled
        | UpdateKind::Msg
        | UpdateKind::Step => msg.to_string(),
    }
}

fn plain_grammar_message(kind: UpdateKind, outcome: ItemOutcome, msg: &str) -> String {
    match kind {
        UpdateKind::Err => format!("failed: {msg}"),
        UpdateKind::Cached => "cached".to_string(),
        UpdateKind::Fin => match outcome {
            ItemOutcome::Cached => "cached".to_string(),
            ItemOutcome::Built => "built".to_string(),
            ItemOutcome::Unknown | ItemOutcome::Cancelled => msg.to_string(),
        },
        UpdateKind::MarkBuilt
        | UpdateKind::MarkCached
        | UpdateKind::MarkCancelled
        | UpdateKind::Msg
        | UpdateKind::Step => msg.to_string(),
    }
}

// ---------------------------------------------------------------------------
// DisplayActor
// ---------------------------------------------------------------------------

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

fn current_viewport_height<B: Backend>(
    terminal: &mut ratatui::Terminal<B>,
) -> Result<u16, B::Error> {
    terminal.autoresize()?;
    Ok(terminal.get_frame().area().height.max(1))
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

fn render_final_report<B: Backend>(
    terminal: &mut ratatui::Terminal<B>,
    lines: Vec<Line<'static>>,
) -> Result<(), B::Error> {
    let viewport_height = current_viewport_height(terminal)?;
    let (prefix, suffix) = split_lines_for_viewport(lines, viewport_height);
    insert_lines_before_viewport(terminal, prefix)?;
    draw_lines_in_viewport(terminal, suffix)
}

fn clear_from_cursor_down() -> io::Result<()> {
    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
    )
}

pub struct DisplayActor {
    state: DisplayState,
    next_id: u64,
    plain_name_width: usize,
    plain_progress_started: bool,
    grid: GridCache,
    row_specs: Vec<RowSpec>,
    rows_dirty: bool,
    last_term_width: Option<usize>,
    rx: mpsc::Receiver<DisplayMessage>,
    tx: mpsc::Sender<DisplayMessage>,
}

impl DisplayActor {
    #[must_use]
    pub fn spawn(mode: Mode, build_dir: Arc<PathBuf>, out_dir: Arc<PathBuf>) -> DisplayAddr {
        let (tx, rx) = mpsc::channel(256);
        let actor = Self {
            state: DisplayState::new(mode, build_dir, out_dir),
            next_id: 1,
            plain_name_width: 16,
            plain_progress_started: false,
            grid: GridCache::new(),
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

    async fn run(mut self) {
        if self.state.mode == Mode::Fancy {
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

        // Initial render
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
            // Drain all pending messages before rendering
            let mut shutdown_tx = None;
            while let Ok(msg) = self.rx.try_recv() {
                match msg {
                    DisplayMessage::Shutdown { tx } => {
                        shutdown_tx = Some(tx);
                        break;
                    }
                    other => self.handle_message(other),
                }
            }

            if let Some(tx) = shutdown_tx {
                self.finish_fancy(&mut terminal, tx);
                return;
            }

            // Render current state
            let lines = self.materialize(terminal.size().map_or(80, |s| s.width));
            if let Err(err) = draw_lines(&mut terminal, lines) {
                ratatui::restore();
                println!();
                eprintln!("tsdl: fancy display failed; falling back to plain progress: {err}");
                self.run_plain().await;
                return;
            }

            // Wait for next event
            tokio::select! {
                msg = self.rx.recv() => {
                    match msg {
                        Some(DisplayMessage::Shutdown { tx }) => {
                            self.finish_fancy(&mut terminal, tx);
                            return;
                        }
                        Some(other) => self.handle_message(other),
                        None => break,
                    }
                }
                _ = tick_interval.tick() => {
                    // Next materialize will update live clocks.
                }
            }
        }

        ratatui::restore();
        println!();
    }

    fn finish_fancy<B: Backend<Error = io::Error>>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
        tx: oneshot::Sender<()>,
    ) {
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

    /// Assemble `Vec<Line>` from the grid cache. Rebuilds row order and layout
    /// only when items have been added. Otherwise reuses the cached row order
    /// and recomputes only stale cells (dirty items + running clocks).
    fn materialize(&mut self, term_width: u16) -> Vec<Line<'static>> {
        let term_w = term_width as usize;

        if self.last_term_width != Some(term_w) {
            self.grid.invalidate_column(Column::Msg);
            self.last_term_width = Some(term_w);
        }

        // Rebuild row order if items were added
        if self.rows_dirty {
            self.row_specs = self.state.compute_row_order();
            let new_layout = self.state.compute_layout(&self.row_specs);

            // Invalidate columns whose width changed
            if new_layout.ref_ != self.grid.layout.ref_ {
                self.grid.invalidate_column(Column::GitRef);
            }
            if new_layout.step != self.grid.layout.step {
                self.grid.invalidate_column(Column::Step);
            }
            if new_layout.name != self.grid.layout.name {
                self.grid.invalidate_column(Column::Name);
            }
            if new_layout.fixed_width != self.grid.layout.fixed_width {
                self.grid.invalidate_column(Column::Msg);
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
            let time_stale =
                is_dirty || matches!(info.status(), ItemStatus::New | ItemStatus::InProgress);
            let time = self.grid.cell(item_id, Column::Time, time_stale, || {
                compute_time_cell(&info, &layout)
            });

            let gref = self.grid.cell(item_id, Column::GitRef, is_dirty, || {
                compute_ref_cell(&info, &layout)
            });

            let step = self.grid.cell(item_id, Column::Step, is_dirty, || {
                compute_step_cell(&info, &layout)
            });

            let icon = self
                .grid
                .cell(item_id, Column::Icon, is_dirty, || compute_icon_cell(&info));

            let name = self.grid.cell(item_id, Column::Name, is_dirty, || {
                compute_name_cell(&spec.display_name, spec.indent, &info, &layout)
            });

            let msg = self.grid.cell(item_id, Column::Msg, is_dirty, || {
                compute_msg_cell(&info, &layout, term_w)
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

    // ── Plain mode ────────────────────────────────────────────────────

    async fn run_plain(&mut self) {
        self.print_plain_metadata();

        while let Some(msg) = self.rx.recv().await {
            match msg {
                DisplayMessage::RegisterLanguage {
                    git_ref,
                    name,
                    num_tasks,
                    tx,
                } => {
                    let addr = self.register_repo(name, git_ref, num_tasks);
                    let _ = tx.send(addr);
                }
                DisplayMessage::RegisterGrammar {
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
                DisplayMessage::RegisterReference { git_ref, name } => {
                    self.print_plain_ref(&name, &git_ref.to_string());
                }
                DisplayMessage::Update { id, kind, msg } => {
                    self.apply_update(id, kind, msg);
                    if matches!(
                        kind,
                        UpdateKind::Msg | UpdateKind::MarkCached | UpdateKind::MarkBuilt
                    ) {
                        continue;
                    }
                    if let Some(line) = self.plain_progress_line(id, kind) {
                        self.print_plain_progress(&line);
                    }
                }
                DisplayMessage::Shutdown { tx } => {
                    self.print_plain_summary();
                    let _ = tx.send(());
                    break;
                }
            }
        }
    }

    fn update_plain_name_width(&mut self, name: &str) -> usize {
        self.plain_name_width = self.plain_name_width.max(name.chars().count());
        self.plain_name_width
    }

    fn print_plain_metadata(&self) {
        println!("build: {}", self.state.build_dir.display());
        println!("out:   {}", self.state.out_dir.display());
        println!();
    }

    fn print_plain_ref(&mut self, name: &str, git_ref: &str) {
        let width = self.update_plain_name_width(name);
        println!("{name:<width$} @ {git_ref}");
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

    fn print_plain_summary(&self) {
        let (cached, built, _building, failed, cancelled) = self.state.summary_counts();
        println!();
        println!("✓ {cached} cached  ✓ {built} built  ✗ {cancelled} cancelled  ✗ {failed} failed");
    }

    fn plain_progress_line(&self, id: u64, kind: UpdateKind) -> Option<PlainLine> {
        if let Some(repo) = self.state.repos.get(&id) {
            return Some(PlainLine {
                name: repo.name.to_string(),
                step: repo.step,
                total: repo.total,
                message: plain_repo_message(kind, repo.outcome, &repo.msg),
            });
        }

        self.state.grammars.get(&id).map(|grammar| PlainLine {
            name: format!("{}/{}", grammar.repo, grammar.name),
            step: grammar.step,
            total: grammar.total,
            message: plain_grammar_message(kind, grammar.outcome, &grammar.msg),
        })
    }

    // ── Message handling ──────────────────────────────────────────────

    fn handle_message(&mut self, msg: DisplayMessage) {
        match msg {
            DisplayMessage::RegisterLanguage {
                git_ref,
                name,
                num_tasks,
                tx,
            } => {
                let addr = self.register_repo(name, git_ref, num_tasks);
                let _ = tx.send(addr);
            }
            DisplayMessage::RegisterGrammar {
                git_ref,
                language,
                name,
                num_tasks,
                tx,
            } => {
                let addr = self.register_grammar(language, name, git_ref, num_tasks);
                let _ = tx.send(addr);
            }
            DisplayMessage::RegisterReference { .. } => {}
            DisplayMessage::Update { id, kind, msg } => {
                self.apply_update(id, kind, msg);
            }
            DisplayMessage::Shutdown { tx } => {
                let _ = tx.send(());
            }
        }
    }

    fn register_repo(&mut self, name: Arc<str>, git_ref: GitRef, num_tasks: usize) -> ProgressAddr {
        let id = self.next_id;
        self.next_id += 1;

        self.state.repos.insert(
            id,
            RepoEntry {
                name,
                git_ref,
                status: ItemStatus::New,
                outcome: ItemOutcome::Unknown,
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

    fn register_grammar(
        &mut self,
        language: Arc<str>,
        name: Arc<str>,
        git_ref: GitRef,
        num_tasks: usize,
    ) -> ProgressAddr {
        let id = self.next_id;
        self.next_id += 1;

        let repo_id = self
            .state
            .repos
            .iter()
            .find(|(_, r)| r.name == language)
            .map_or(0, |(id, _)| *id);

        self.state.grammars.insert(
            id,
            GrammarEntry {
                repo: language,
                repo_id,
                name,
                git_ref,
                status: ItemStatus::New,
                outcome: ItemOutcome::Unknown,
                msg: Arc::from(""),
                step: 0,
                total: num_tasks,
                started_at: Instant::now(),
                frozen_elapsed: None,
            },
        );

        self.rows_dirty = true;
        if repo_id != 0 {
            self.sync_parent_repo(repo_id);
        }

        ProgressAddr {
            id,
            tx: self.tx.clone(),
        }
    }

    fn apply_update(&mut self, id: u64, kind: UpdateKind, msg: Arc<str>) {
        if let Some(repo) = self.state.repos.get_mut(&id) {
            Self::apply_repo_update(repo, kind, msg);
            self.grid.mark_dirty(id);
            return;
        }

        let mut maybe_parent_id: Option<u64> = None;
        if let Some(grammar) = self.state.grammars.get_mut(&id) {
            Self::apply_grammar_update(grammar, kind, msg);
            self.grid.mark_dirty(id);
            if matches!(
                kind,
                UpdateKind::Step
                    | UpdateKind::MarkCached
                    | UpdateKind::MarkBuilt
                    | UpdateKind::MarkCancelled
                    | UpdateKind::Cached
                    | UpdateKind::Fin
                    | UpdateKind::Err
            ) {
                maybe_parent_id = Some(grammar.repo_id);
            }
        }

        if let Some(repo_id) = maybe_parent_id {
            self.sync_parent_repo(repo_id);
        }
    }

    fn apply_repo_update(repo: &mut RepoEntry, kind: UpdateKind, msg: Arc<str>) {
        match kind {
            UpdateKind::Msg => {
                repo.msg = msg;
            }
            UpdateKind::Step => {
                repo.status = ItemStatus::InProgress;
                repo.step += 1;
                repo.msg = msg;
            }
            UpdateKind::MarkCached => {
                repo.outcome = ItemOutcome::Cached;
            }
            UpdateKind::MarkCancelled => {
                repo.frozen_elapsed = Some(repo.started_at.elapsed());
                repo.status = ItemStatus::Cancelled;
                repo.outcome = ItemOutcome::Cancelled;
                if repo.total > 0 {
                    repo.step = repo.total;
                }
                repo.msg = msg;
            }

            UpdateKind::MarkBuilt => {
                repo.outcome = ItemOutcome::Built;
            }
            UpdateKind::Cached => {
                repo.frozen_elapsed = Some(repo.started_at.elapsed());
                repo.status = ItemStatus::Done;
                repo.outcome = ItemOutcome::Cached;
                if repo.total > 0 {
                    repo.step = repo.total;
                }
                repo.msg = msg;
            }
            UpdateKind::Fin => {
                repo.frozen_elapsed = Some(repo.started_at.elapsed());
                repo.status = ItemStatus::Done;
                if repo.outcome == ItemOutcome::Unknown {
                    repo.outcome = ItemOutcome::Built;
                }
                if repo.total > 0 {
                    repo.step = repo.total;
                }
                repo.msg = msg;
            }
            UpdateKind::Err => {
                repo.frozen_elapsed = Some(repo.started_at.elapsed());
                repo.status = ItemStatus::Failed;
                repo.msg = msg;
            }
        }
    }

    fn apply_grammar_update(grammar: &mut GrammarEntry, kind: UpdateKind, msg: Arc<str>) {
        match kind {
            UpdateKind::Msg => {
                grammar.msg = msg;
            }
            UpdateKind::Step => {
                grammar.status = ItemStatus::InProgress;
                grammar.step += 1;
                grammar.msg = msg;
            }
            UpdateKind::MarkCached => {
                grammar.outcome = ItemOutcome::Cached;
            }
            UpdateKind::MarkCancelled => {
                grammar.frozen_elapsed = Some(grammar.started_at.elapsed());
                grammar.status = ItemStatus::Cancelled;
                grammar.outcome = ItemOutcome::Cancelled;
                grammar.step = grammar.total;
                grammar.msg = msg;
            }

            UpdateKind::MarkBuilt => {
                grammar.outcome = ItemOutcome::Built;
            }
            UpdateKind::Cached => {
                grammar.frozen_elapsed = Some(grammar.started_at.elapsed());
                grammar.status = ItemStatus::Done;
                grammar.outcome = ItemOutcome::Cached;
                grammar.step = grammar.total;
                grammar.msg = msg;
            }
            UpdateKind::Fin => {
                grammar.frozen_elapsed = Some(grammar.started_at.elapsed());
                grammar.status = ItemStatus::Done;
                if grammar.outcome == ItemOutcome::Unknown {
                    grammar.outcome = ItemOutcome::Built;
                }
                grammar.step = grammar.total;
                grammar.msg = msg;
            }
            UpdateKind::Err => {
                grammar.frozen_elapsed = Some(grammar.started_at.elapsed());
                grammar.status = ItemStatus::Failed;
                grammar.msg = msg;
            }
        }
    }

    fn sync_parent_repo(&mut self, repo_id: u64) {
        let has_any = self.state.grammars.values().any(|g| g.repo_id == repo_id);
        if !has_any {
            return;
        }

        let any_failed = self
            .state
            .grammars
            .values()
            .any(|g| g.repo_id == repo_id && g.status == ItemStatus::Failed);
        let any_cancelled = self
            .state
            .grammars
            .values()
            .any(|g| g.repo_id == repo_id && g.status == ItemStatus::Cancelled);
        let any_active = self.state.grammars.values().any(|g| {
            g.repo_id == repo_id
                && (g.status == ItemStatus::New || g.status == ItemStatus::InProgress)
        });
        let outcome = self.aggregate_child_outcome(repo_id);

        if let Some(repo) = self.state.repos.get_mut(&repo_id) {
            repo.outcome = outcome;
            if any_active {
                repo.status = ItemStatus::InProgress;
                repo.msg = Arc::from("building");
                repo.frozen_elapsed = None;
            } else if any_failed {
                repo.status = ItemStatus::Failed;
                repo.msg = Arc::from("failed");
                repo.frozen_elapsed
                    .get_or_insert_with(|| repo.started_at.elapsed());
            } else if any_cancelled {
                repo.status = ItemStatus::Cancelled;
                repo.msg = Arc::from("cancelled");
                repo.frozen_elapsed
                    .get_or_insert_with(|| repo.started_at.elapsed());
            } else {
                repo.status = ItemStatus::Done;
                repo.msg = Arc::from("done");
                repo.frozen_elapsed
                    .get_or_insert_with(|| repo.started_at.elapsed());
            }
        }

        self.grid.mark_dirty(repo_id);
    }

    fn aggregate_child_outcome(&self, repo_id: u64) -> ItemOutcome {
        let mut saw_cached = false;
        let mut saw_cancelled = false;
        let mut saw_unknown = false;

        for grammar in self
            .state
            .grammars
            .values()
            .filter(|g| g.repo_id == repo_id)
        {
            match grammar.outcome {
                ItemOutcome::Built => return ItemOutcome::Built,
                ItemOutcome::Cached => saw_cached = true,
                ItemOutcome::Cancelled => saw_cancelled = true,
                ItemOutcome::Unknown => saw_unknown = true,
            }
        }

        if saw_unknown {
            ItemOutcome::Unknown
        } else if saw_cancelled {
            ItemOutcome::Cancelled
        } else if saw_cached {
            ItemOutcome::Cached
        } else {
            ItemOutcome::Unknown
        }
    }
}

fn spacer() -> Span<'static> {
    Span::raw(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> DisplayActor {
        let (tx, rx) = mpsc::channel(1);
        DisplayActor {
            state: DisplayState::new(
                Mode::Fancy,
                Arc::new(PathBuf::from("build")),
                Arc::new(PathBuf::from("out")),
            ),
            next_id: 1,
            plain_name_width: 16,
            plain_progress_started: false,
            grid: GridCache::new(),
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

        let cached = actor.register_repo("tree-sitter-cli".into(), GitRef::from("HEAD"), 2);
        actor.apply_update(cached.id, UpdateKind::MarkCached, Arc::from(""));
        actor.apply_update(cached.id, UpdateKind::Fin, Arc::from("done"));

        let built = actor.register_repo("standalone".into(), GitRef::from("HEAD"), 1);
        actor.apply_update(built.id, UpdateKind::MarkBuilt, Arc::from(""));
        actor.apply_update(built.id, UpdateKind::Fin, Arc::from("done"));

        let active = actor.register_repo("active".into(), GitRef::from("HEAD"), 1);
        actor.apply_update(active.id, UpdateKind::Step, Arc::from("working"));

        let failed = actor.register_repo("failed".into(), GitRef::from("HEAD"), 1);
        actor.apply_update(failed.id, UpdateKind::Err, Arc::from("failed"));

        assert_eq!(actor.state.summary_counts(), (1, 1, 1, 1, 0));
    }

    #[test]
    fn summary_counts_do_not_double_count_parent_repos_with_grammars() {
        let mut actor = actor();

        let repo = actor.register_repo("json".into(), GitRef::from("HEAD"), 2);
        actor.apply_update(repo.id, UpdateKind::MarkBuilt, Arc::from(""));
        actor.apply_update(repo.id, UpdateKind::Fin, Arc::from("done"));

        let grammar = actor.register_grammar("json".into(), "json".into(), GitRef::from("HEAD"), 4);
        actor.apply_update(grammar.id, UpdateKind::MarkCached, Arc::from(""));
        actor.apply_update(grammar.id, UpdateKind::Cached, Arc::from("done"));

        assert_eq!(actor.state.summary_counts(), (1, 0, 0, 0, 0));
    }

    #[test]
    fn parent_repo_stays_active_while_any_child_is_active() {
        let mut actor = actor();

        let repo = actor.register_repo("typescript".into(), GitRef::from("HEAD"), 2);
        let failing = actor.register_grammar(
            "typescript".into(),
            "typescript".into(),
            GitRef::from("HEAD"),
            4,
        );
        let pending =
            actor.register_grammar("typescript".into(), "tsx".into(), GitRef::from("HEAD"), 4);

        actor.apply_update(failing.id, UpdateKind::Step, Arc::from("building"));
        actor.apply_update(failing.id, UpdateKind::Err, Arc::from("build failed"));

        let repo_entry = actor.state.repos.get(&repo.id).unwrap();
        assert_eq!(repo_entry.status, ItemStatus::InProgress);
        assert_eq!(repo_entry.msg.as_ref(), "building");
        assert!(repo_entry.frozen_elapsed.is_none());

        actor.apply_update(pending.id, UpdateKind::MarkBuilt, Arc::from(""));
        actor.apply_update(pending.id, UpdateKind::Fin, Arc::from("done"));

        let repo_entry = actor.state.repos.get(&repo.id).unwrap();
        assert_eq!(repo_entry.status, ItemStatus::Failed);
        assert_eq!(repo_entry.msg.as_ref(), "failed");
        assert!(repo_entry.frozen_elapsed.is_some());
    }

    #[test]
    fn materialize_recomputes_message_cells_when_terminal_width_changes() {
        let mut actor = actor();
        let progress = actor.register_repo("json".into(), GitRef::from("HEAD"), 1);
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
        let progress = actor.register_repo("json".into(), GitRef::from("HEAD"), 1);
        let message = "abcdefghijklmnop";

        actor.apply_update(progress.id, UpdateKind::Msg, Arc::from(message));

        let initial_lines = actor.materialize(42);
        let initial_row = line_text(&initial_lines[0]);
        assert!(initial_row.contains(message));

        actor.register_repo("zzzzzzzzzzzzzzzzzzzz".into(), GitRef::from("HEAD"), 1);

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
