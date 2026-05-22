use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
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

    /// Advance the elapsed-time display and trigger a re-render.
    Tick,

    /// Flush and close the display actor. The response is sent after cleanup.
    Shutdown { tx: oneshot::Sender<()> },
}

#[derive(Debug, Clone, Copy)]
pub enum UpdateKind {
    Msg,
    Step,
    MarkCached,
    MarkBuilt,
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

    pub async fn tick(&self) {
        self.fire(DisplayMessage::Tick).await;
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
            ItemOutcome::Cached => "done".to_string(),
            ItemOutcome::Built => "done".to_string(),
            ItemOutcome::Unknown => msg.to_string(),
        },
        UpdateKind::Step => msg.to_string(),
        UpdateKind::Msg | UpdateKind::MarkCached | UpdateKind::MarkBuilt => msg.to_string(),
    }
}

fn plain_grammar_message(kind: UpdateKind, outcome: ItemOutcome, msg: &str) -> String {
    match kind {
        UpdateKind::Err => format!("failed: {msg}"),
        UpdateKind::Cached => "cached".to_string(),
        UpdateKind::Fin => match outcome {
            ItemOutcome::Cached => "cached".to_string(),
            ItemOutcome::Built => "built".to_string(),
            ItemOutcome::Unknown => msg.to_string(),
        },
        UpdateKind::Step => msg.to_string(),
        UpdateKind::Msg | UpdateKind::MarkCached | UpdateKind::MarkBuilt => msg.to_string(),
    }
}

// ---------------------------------------------------------------------------
// DisplayActor
// ---------------------------------------------------------------------------

pub struct DisplayActor {
    state: DisplayState,
    next_id: u64,
    plain_name_width: usize,
    plain_progress_started: bool,
    grid: GridCache,
    row_specs: Vec<RowSpec>,
    rows_dirty: bool,
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
        let term_height = crossterm::terminal::size()
            .map_or(40, |(_, h)| h as usize);
        let viewport_height = term_height.min(40);

        let mut terminal = ratatui::Terminal::with_options(
            ratatui::backend::CrosstermBackend::new(io::stderr()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Inline(viewport_height as u16),
            },
        )
        .expect("Failed to initialize ratatui terminal");

        // Initial render
        let lines = self.materialize(terminal.size().map_or(80, |s| s.width));
        let _ = terminal.draw(|frame| {
            let area = frame.area();
            frame.render_widget(Paragraph::new(lines), area);
        });

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
                // Final render before shutting down
                let lines = self.materialize(terminal.size().map_or(80, |s| s.width));
                let _ = terminal.draw(|frame| {
                    let area = frame.area();
                    frame.render_widget(Paragraph::new(lines), area);
                });
                ratatui::restore();
                eprintln!();
                let _ = tx.send(());
                return;
            }

            // Render current state
            let lines = self.materialize(terminal.size().map_or(80, |s| s.width));
            let _ = terminal.draw(|frame| {
                let area = frame.area();
                frame.render_widget(Paragraph::new(lines), area);
            });

            // Wait for next event
            tokio::select! {
                msg = self.rx.recv() => {
                    match msg {
                        Some(DisplayMessage::Shutdown { tx }) => {
                            // Final render then exit
                            let lines = self.materialize(terminal.size().map_or(80, |s| s.width));
                            let _ = terminal.draw(|frame| {
                                let area = frame.area();
                                frame.render_widget(Paragraph::new(lines), area);
                            });
                            ratatui::restore();
                            eprintln!();
                            let _ = tx.send(());
                            return;
                        }
                        Some(other) => self.handle_message(other),
                        None => break,
                    }
                }
                _ = tick_interval.tick() => {
                    // Tick — next materialize will update live clocks
                }
            }
        }

        ratatui::restore();
        eprintln!();
    }

    /// Assemble `Vec<Line>` from the grid cache. Rebuilds row order and layout
    /// only when items have been added. Otherwise reuses the cached row order
    /// and recomputes only stale cells (dirty items + running clocks).
    fn materialize(&mut self, term_width: u16) -> Vec<Line<'static>> {
        let term_w = term_width as usize;

        // Rebuild row order if items were added
        if self.rows_dirty {
            self.row_specs = self.state.compute_row_order();
            let new_layout = self.state.compute_layout(&self.row_specs);

            // Invalidate columns whose width changed
            if new_layout.ref_width != self.grid.layout.ref_width {
                self.grid.invalidate_column(Column::GitRef);
            }
            if new_layout.step_width != self.grid.layout.step_width {
                self.grid.invalidate_column(Column::Step);
            }
            if new_layout.name_width != self.grid.layout.name_width {
                self.grid.invalidate_column(Column::Name);
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
                DisplayMessage::Tick => {}
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
        let (cached, built, _building, failed) = self.state.summary_counts();
        println!();
        println!("✓ {cached} cached  ✓ {built} built  ✗ {failed} failed");
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
            DisplayMessage::Tick => {}
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
        let any_active = self.state.grammars.values().any(|g| {
            g.repo_id == repo_id
                && (g.status == ItemStatus::New || g.status == ItemStatus::InProgress)
        });
        let outcome = self.aggregate_child_outcome(repo_id);

        if let Some(repo) = self.state.repos.get_mut(&repo_id) {
            repo.outcome = outcome;
            if any_failed {
                repo.status = ItemStatus::Failed;
                repo.msg = Arc::from("failed");
                repo.frozen_elapsed
                    .get_or_insert_with(|| repo.started_at.elapsed());
            } else if any_active {
                repo.status = ItemStatus::InProgress;
                repo.msg = Arc::from("building");
                repo.frozen_elapsed = None;
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
                ItemOutcome::Unknown => saw_unknown = true,
            }
        }

        if saw_unknown {
            ItemOutcome::Unknown
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
