//! TUI application state machine and main event loop.
//!
//! Phase 4: search + symbol detail (Overview / Callers / Callees / Source tabs).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use atlas_engine::{CallerChain, ContextView, SearchResult, Store};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::event::{Event, EventHandler};
use super::jobs::{JobManager, JobResult, TuiJob};
use super::search_session::{ParsedSearch, parse_query};
use super::session::GraphSession;
use super::widgets::context_view::DetailTab;
use super::widgets::{context_view, results_list, search_bar, status_bar, trace_view};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

const EXIT_CONFIRM_DURATION: Duration = Duration::from_secs(1);

/// Which area of the UI currently receives keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    SearchBar,
    Results,
    Detail,
}

/// Top-level screen state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    SearchHome,
    SymbolDetail,
    TraceView,
}

/// Main TUI application state.
pub struct App {
    pub should_quit: bool,
    pub store: Arc<Store>,
    pub project_root: PathBuf,
    pub session: GraphSession,

    // ── Search state ──────────────────────────────────────────────────
    pub search_input: String,
    pub search_cursor: usize,
    pub search_results: Vec<SearchResult>,
    pub selected_index: usize,
    pub focus: Focus,

    // ── Detail state ──────────────────────────────────────────────────
    screen: Screen,
    detail_tab: DetailTab,
    detail_context: Option<ContextView>,
    detail_selected: usize, // for caller/callee lists
    detail_scroll: usize,

    // ── Trace state ────────────────────────────────────────────────────
    trace_chain: Option<CallerChain>,
    trace_selected: usize,
    trace_scroll: usize,

    exit_confirm_until: Option<Instant>,

    // ── Job system ────────────────────────────────────────────────────
    job_manager: JobManager,
    /// Stashed parsed search for re-submission after lazy structural.
    pending_search: Option<ParsedSearch>,
    /// Whether lazy structural has been triggered for the current search.
    search_lazy_triggered: bool,

    // ── DB stats (cached once) ────────────────────────────────────────
    file_count: i64,
    symbol_count: i64,
    edge_count: i64,
    index_mode: String,
}

impl App {
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        let stats = store.get_stats().ok();
        let (file_count, symbol_count, edge_count) = stats
            .map(|s| (s.total_files, s.total_symbols, s.total_edges))
            .unwrap_or_default();
        let index_mode = detect_index_mode(&store);

        let session = GraphSession::new(Arc::clone(&store), project_root.clone());
        let job_manager = JobManager::new(Arc::clone(&store), project_root.clone());

        Self {
            should_quit: false,
            store,
            project_root,
            session,
            exit_confirm_until: None,
            job_manager,
            pending_search: None,
            search_lazy_triggered: false,
            search_input: String::new(),
            search_cursor: 0,
            search_results: Vec::new(),
            selected_index: 0,
            focus: Focus::SearchBar,
            screen: Screen::SearchHome,
            detail_tab: DetailTab::Overview,
            detail_context: None,
            detail_selected: 0,
            detail_scroll: 0,
            trace_chain: None,
            trace_selected: 0,
            trace_scroll: 0,
            file_count,
            symbol_count,
            edge_count,
            index_mode,
        }
    }

    pub fn run(
        &mut self,
        terminal: &mut ratatui::Terminal<impl ratatui::backend::Backend>,
    ) -> anyhow::Result<()> {
        let tick_rate = Duration::from_millis(250);
        let event_handler = EventHandler::new(tick_rate);
        terminal.draw(|f| self.render(f))?;

        while !self.should_quit {
            let event = event_handler.next()?;
            self.handle_event(event);
            terminal.draw(|f| self.render(f))?;
        }
        Ok(())
    }

    // ── event dispatch ────────────────────────────────────────────────────

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key_press(key),
            Event::Key(_) => {}
            Event::Resize(_, _) => {}
            Event::Tick => self.handle_tick(),
        }
    }

    fn handle_tick(&mut self) {
        if self
            .exit_confirm_until
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.exit_confirm_until = None;
        }

        // ── Job polling (search / trace / lazy structural) ────────────────
        match self.job_manager.poll() {
            Some(super::jobs::JobStatus::Completed { result }) => {
                self.handle_job_completion(result);
            }
            Some(super::jobs::JobStatus::Cancelled) => {
                self.pending_search = None;
                self.search_lazy_triggered = false;
            }
            _ => {}
        }
    }

    fn handle_key_press(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }

        if key.code != KeyCode::Esc {
            self.clear_exit_confirmation();
        }

        let code = key.code;
        match self.screen {
            Screen::SearchHome => self.handle_search_key(code),
            Screen::SymbolDetail => self.handle_detail_key(code),
            Screen::TraceView => self.handle_trace_key(code),
        }
    }

    // ── SearchHome key handling ───────────────────────────────────────────

    fn handle_search_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                // If a background job is running, cancel it.
                if self.job_manager.is_running() {
                    self.job_manager.cancel_current();
                    return;
                }
                if self.focus == Focus::Results || !self.search_input.is_empty() {
                    self.reset_search_input();
                    self.clear_exit_confirmation();
                } else if self.exit_confirmation_active() {
                    self.should_quit = true;
                } else {
                    self.request_exit_confirmation();
                }
            }

            // ── search bar input ────────────────────────────────────────
            KeyCode::Enter if self.focus == Focus::SearchBar => self.perform_search(),
            KeyCode::Char(c) if self.focus == Focus::SearchBar => {
                search_bar::handle_key(&mut self.search_input, &mut self.search_cursor, c);
            }
            KeyCode::Backspace if self.focus == Focus::SearchBar => {
                if self.search_cursor > 0 {
                    let bp = byte_index_at_char(&self.search_input, self.search_cursor - 1);
                    self.search_input.remove(bp);
                    self.search_cursor -= 1;
                }
            }
            KeyCode::Left if self.focus == Focus::SearchBar => {
                self.search_cursor = self.search_cursor.saturating_sub(1);
            }
            KeyCode::Right if self.focus == Focus::SearchBar => {
                self.search_cursor =
                    (self.search_cursor + 1).min(self.search_input.chars().count());
            }
            KeyCode::Home if self.focus == Focus::SearchBar => self.search_cursor = 0,
            KeyCode::End if self.focus == Focus::SearchBar => {
                self.search_cursor = self.search_input.chars().count();
            }

            // ── focus switch ────────────────────────────────────────────
            KeyCode::Char('/') => self.focus = Focus::SearchBar,

            // ── results navigation ──────────────────────────────────────
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus = Focus::Results;
                if !self.search_results.is_empty() {
                    self.selected_index =
                        (self.selected_index + 1).min(self.search_results.len() - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus = Focus::Results;
                self.selected_index = self.selected_index.saturating_sub(1);
            }
            KeyCode::PageDown => {
                self.focus = Focus::Results;
                if !self.search_results.is_empty() {
                    self.selected_index =
                        (self.selected_index + 10).min(self.search_results.len() - 1);
                }
            }
            KeyCode::PageUp => {
                self.focus = Focus::Results;
                self.selected_index = self.selected_index.saturating_sub(10);
            }
            KeyCode::Enter if !self.search_results.is_empty() => {
                self.open_symbol_detail();
            }

            _ => {}
        }
    }

    // ── TraceView key handling ──────────────────────────────────────────────

    fn handle_trace_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                // Back to SymbolDetail.
                self.screen = Screen::SymbolDetail;
                self.focus = Focus::Detail;
                self.trace_chain = None;
                self.trace_selected = 0;
                self.trace_scroll = 0;
            }
            KeyCode::Char('/') => {
                // Jump back to search.
                self.screen = Screen::SearchHome;
                self.focus = Focus::SearchBar;
                self.search_input.clear();
                self.search_cursor = 0;
                self.trace_chain = None;
                self.trace_selected = 0;
                self.trace_scroll = 0;
            }

            // ── navigation within trace ────────────────────────────────────
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(ref chain) = self.trace_chain {
                    let max = chain.steps.len().saturating_sub(1);
                    self.trace_selected = (self.trace_selected + 1).min(max);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.trace_selected = self.trace_selected.saturating_sub(1);
            }
            KeyCode::PageDown => {
                if let Some(ref chain) = self.trace_chain {
                    let max = chain.steps.len().saturating_sub(1);
                    self.trace_selected = (self.trace_selected + 10).min(max);
                }
            }
            KeyCode::PageUp => {
                self.trace_selected = self.trace_selected.saturating_sub(10);
            }

            _ => {}
        }
    }

    // ── SymbolDetail key handling ─────────────────────────────────────────

    fn handle_detail_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                // Back to the search results list, preserving the query.
                self.return_to_results();
            }
            KeyCode::Tab => {
                self.detail_tab = self.detail_tab.next();
                self.detail_selected = 0;
                self.detail_scroll = 0;
            }
            KeyCode::Char('t') => {
                // Submit a background trace job (non-blocking).
                if let Some(ctx) = &self.detail_context {
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.job_manager.submit(TuiJob::TraceCallers {
                        symbol_id: ctx.subject.id.clone(),
                        depth: 20,
                        cancel,
                    });
                }
            }
            KeyCode::Char('/') => {
                // Jump back to search.
                self.screen = Screen::SearchHome;
                self.focus = Focus::SearchBar;
                self.search_input.clear();
                self.search_cursor = 0;
                self.detail_context = None;
            }

            // ── navigation within detail ───────────────────────────────
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus = Focus::Detail;
                let max = self.detail_list_len().saturating_sub(1);
                self.detail_selected = (self.detail_selected + 1).min(max);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus = Focus::Detail;
                self.detail_selected = self.detail_selected.saturating_sub(1);
            }
            KeyCode::PageDown => {
                self.focus = Focus::Detail;
                let max = self.detail_list_len().saturating_sub(1);
                self.detail_selected = (self.detail_selected + 10).min(max);
            }
            KeyCode::PageUp => {
                self.focus = Focus::Detail;
                self.detail_selected = self.detail_selected.saturating_sub(10);
            }
            KeyCode::Enter => {
                // Navigate to the selected caller or callee.
                self.navigate_to_selected_callee_or_caller();
            }

            _ => {}
        }
    }

    // ── navigation ────────────────────────────────────────────────────────

    fn open_symbol_detail(&mut self) {
        if self.selected_index >= self.search_results.len() {
            return;
        }
        let symbol = &self.search_results[self.selected_index].symbol;

        // Ensure graph is initialized.
        if let Err(e) = self.session.ensure_initialized() {
            tracing::error!("Failed to init graph: {e}");
            return;
        }

        match self
            .session
            .context_builder()
            .build_context_for_symbol(&symbol.id, true)
        {
            Ok(ctx) => {
                self.detail_context = Some(ctx);
                self.detail_tab = DetailTab::Overview;
                self.detail_selected = 0;
                self.detail_scroll = 0;
                self.screen = Screen::SymbolDetail;
                self.focus = Focus::Detail;
            }
            Err(e) => {
                tracing::error!("Failed to build context: {e}");
            }
        }
    }

    fn navigate_to_selected_callee_or_caller(&mut self) {
        let ctx = match &self.detail_context {
            Some(c) => c,
            None => return,
        };

        let target = match self.detail_tab {
            DetailTab::Callers => ctx
                .caller_details
                .get(self.detail_selected)
                .map(|c| c.symbol.id),
            DetailTab::Callees => ctx
                .callee_details
                .get(self.detail_selected)
                .map(|c| c.symbol.id),
            DetailTab::Peers => ctx.file_peers.get(self.detail_selected).map(|p| p.id),
            _ => return,
        };

        if let Some(symbol_id) = target {
            match self
                .session
                .context_builder()
                .build_context_for_symbol(&symbol_id, true)
            {
                Ok(ctx) => {
                    self.detail_context = Some(ctx);
                    self.detail_selected = 0;
                    self.detail_scroll = 0;
                }
                Err(e) => tracing::error!("Failed to navigate: {e}"),
            }
        }
    }

    fn detail_list_len(&self) -> usize {
        match &self.detail_context {
            Some(ctx) => match self.detail_tab {
                DetailTab::Callers => ctx.caller_details.len(),
                DetailTab::Callees => ctx.callee_details.len(),
                DetailTab::Peers => ctx.file_peers.len(),
                _ => 0,
            },
            None => 0,
        }
    }

    fn reset_search_input(&mut self) {
        self.screen = Screen::SearchHome;
        self.focus = Focus::SearchBar;
        self.search_input.clear();
        self.search_cursor = 0;
        self.search_results.clear();
        self.selected_index = 0;
    }

    fn exit_confirmation_active(&self) -> bool {
        self.exit_confirm_until
            .is_some_and(|deadline| Instant::now() < deadline)
    }

    fn request_exit_confirmation(&mut self) {
        self.exit_confirm_until = Some(Instant::now() + EXIT_CONFIRM_DURATION);
    }

    fn clear_exit_confirmation(&mut self) {
        self.exit_confirm_until = None;
    }

    fn return_to_results(&mut self) {
        self.screen = Screen::SearchHome;
        self.focus = Focus::Results;
        self.detail_context = None;
        self.detail_selected = 0;
        self.detail_scroll = 0;
    }

    // ── search ────────────────────────────────────────────────────────────

    fn perform_search(&mut self) {
        if self.search_input.trim().is_empty() {
            self.search_results.clear();
            self.selected_index = 0;
            return;
        }
        if let Err(e) = self.session.ensure_initialized() {
            tracing::error!("Failed to init graph: {e}");
            return;
        }
        if let Err(e) = self.session.maybe_refresh() {
            tracing::error!("Failed to refresh graph: {e}");
            return;
        }

        // Push current graph snapshot to the job manager so background
        // workers can construct their own SearchEngine from it.
        self.job_manager
            .set_graph(self.session.graph_engine().clone());

        let parsed = parse_query(&self.search_input);
        self.pending_search = Some(parsed.clone());
        self.search_lazy_triggered = false;

        let cancel = Arc::new(AtomicBool::new(false));
        self.job_manager.submit(TuiJob::Search {
            query: self.search_input.clone(),
            scope: parsed.scope_path.clone(),
            language: parsed.language,
            cancel,
        });
    }

    /// Handle a completed background job.
    fn handle_job_completion(&mut self, result: JobResult) {
        match result {
            JobResult::SearchResults(results) => {
                let count = results.len();
                self.search_results = results;
                self.selected_index = 0;
                self.focus = Focus::Results;
                self.pending_search = None;
                self.search_lazy_triggered = false;
                tracing::info!("Search returned {count} results");
            }
            JobResult::SearchEmpty => {
                if self.search_lazy_triggered {
                    // Already tried lazy structural — accept empty.
                    self.search_results.clear();
                    self.selected_index = 0;
                    self.focus = Focus::Results;
                    self.pending_search = None;
                    self.search_lazy_triggered = false;
                } else {
                    // Trigger lazy structural extraction.
                    let search_term = self
                        .pending_search
                        .as_ref()
                        .map(|p| p.search_term.clone())
                        .unwrap_or_default();
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.job_manager.submit(TuiJob::LazyStructural {
                        search_term,
                        cancel,
                    });
                    self.search_lazy_triggered = true;
                }
            }
            JobResult::LazyComplete {
                files_built,
                files_cached,
            } => {
                tracing::info!("Lazy structural: {files_built} built, {files_cached} cached");
                // Refresh the graph on the main thread so it picks up the
                // newly extracted symbols.
                self.session.mark_stale();
                if let Err(e) = self.session.maybe_refresh() {
                    tracing::error!("Failed to refresh graph after lazy structural: {e}");
                }
                self.refresh_cached_status();
                // Push refreshed graph to the job manager.
                self.job_manager
                    .set_graph(self.session.graph_engine().clone());
                // Re-submit the original search with the refreshed graph.
                if let Some(ref parsed) = self.pending_search {
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.job_manager.submit(TuiJob::Search {
                        query: parsed.search_term.clone(),
                        scope: parsed.scope_path.clone(),
                        language: parsed.language,
                        cancel,
                    });
                }
            }
            JobResult::TraceChain(chain) => {
                self.trace_chain = chain;
                self.trace_selected = 0;
                self.trace_scroll = 0;
                self.screen = Screen::TraceView;
            }
        }
    }

    // ── render ────────────────────────────────────────────────────────────

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        let v_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        let search_area = v_chunks[0];
        let body_area = v_chunks[1];
        let status_area = v_chunks[2];

        // Search bar.
        search_bar::render(
            frame,
            search_area,
            &self.search_input,
            self.search_cursor,
            self.focus == Focus::SearchBar,
        );

        // Body: left 40% / right 60%.
        let body_cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(body_area);

        // Results list (visible in both screens).
        let rows: Vec<results_list::ResultRow> = self
            .search_results
            .iter()
            .cloned()
            .map(Into::into)
            .collect();

        let mut scroll = 0;
        if !self.search_results.is_empty() {
            results_list::render(frame, body_cols[0], &rows, self.selected_index, &mut scroll);
        } else {
            let hint = "Type a query and press Enter to search";
            let p = Paragraph::new(hint).style(Style::default().fg(Color::DarkGray));
            frame.render_widget(p, centered_in(body_cols[0], hint.len() as u16, 1));
        }

        // Right panel.
        match (&self.screen, &self.detail_context, &self.trace_chain) {
            (Screen::SymbolDetail, Some(ctx), _) => {
                // Clamp scroll before render so selected stays visible.
                let available_height = body_cols[1].height.saturating_sub(2) as usize;
                if self.detail_selected < self.detail_scroll {
                    self.detail_scroll = self.detail_selected;
                } else if self.detail_selected >= self.detail_scroll + available_height {
                    self.detail_scroll = self.detail_selected.saturating_sub(available_height - 1);
                }
                let active_tab = self.detail_tab;
                context_view::render(
                    frame,
                    body_cols[1],
                    ctx,
                    active_tab,
                    self.detail_selected,
                    self.detail_scroll,
                );
            }
            (Screen::TraceView, _, Some(chain)) => {
                // Clamp trace scroll before render.
                let chain_height = body_cols[1].height.saturating_sub(2) as usize;
                if self.trace_selected < self.trace_scroll {
                    self.trace_scroll = self.trace_selected;
                } else if self.trace_selected >= self.trace_scroll + chain_height {
                    self.trace_scroll = self.trace_selected.saturating_sub(chain_height - 1);
                }
                trace_view::render(
                    frame,
                    body_cols[1],
                    chain,
                    self.trace_selected,
                    self.trace_scroll,
                );
            }
            _ => {
                let hint = if self.session.is_initialized() {
                    "Select a result (Enter) to view details"
                } else {
                    "Graph loading..."
                };
                let p = Paragraph::new(hint).style(Style::default().fg(Color::DarkGray));
                frame.render_widget(p, centered_in(body_cols[1], hint.len() as u16, 1));
            }
        }

        // Status bar.
        status_bar::render(
            frame,
            status_area,
            self.file_count,
            self.symbol_count,
            self.edge_count,
            self.session.is_initialized(),
            &format!("Index: {}", self.index_mode),
        );

        if self.exit_confirmation_active() {
            render_exit_confirmation(frame, area);
        } else {
            self.clear_exit_confirmation();
        }
    }

    fn refresh_cached_status(&mut self) {
        if let Ok(stats) = self.store.get_stats() {
            self.file_count = stats.total_files;
            self.symbol_count = stats.total_symbols;
            self.edge_count = stats.total_edges;
            self.index_mode = detect_index_mode(&self.store);
        }
    }
}

/// Detect index mode by delegating to the canonical `Store::read_index_mode()`.
///
/// This is the single source of truth for index-mode detection, shared by
/// CLI, MCP, and TUI.  Previously the TUI maintained its own divergent
/// detection logic; see issue 2.1 in the pre-release review.
fn detect_index_mode(store: &Store) -> String {
    store.read_index_mode().unwrap_or_else(|_| "unknown".into())
}

// ── helpers ───────────────────────────────────────────────────────────────

fn render_exit_confirmation(frame: &mut Frame, area: Rect) {
    let popup = centered_in(area, 28, 3);
    frame.render_widget(Clear, popup);

    let prompt = Paragraph::new("Press ESC again to confirm exit")
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Yellow))
        .alignment(Alignment::Center);
    frame.render_widget(prompt, popup);
}

fn byte_index_at_char(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn centered_in(area: Rect, width: u16, height: u16) -> Rect {
    let pct = if area.width > 0 {
        ((width as f32 / area.width as f32) * 100.0) as u16
    } else {
        0
    };
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100u16.saturating_sub(pct)) / 2),
            Constraint::Length(width),
            Constraint::Percentage((100u16.saturating_sub(pct)) / 2),
        ])
        .split(area);
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Length(height),
            Constraint::Percentage(50),
        ])
        .split(h[1]);
    v[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let store = Arc::new(Store::open_in_memory().expect("in-memory store"));
        let project_root = PathBuf::from(".");
        let session = GraphSession::new(Arc::clone(&store), project_root.clone());
        let job_manager = JobManager::new(Arc::clone(&store), project_root.clone());

        App {
            should_quit: false,
            store,
            project_root,
            session,
            job_manager,
            pending_search: None,
            search_lazy_triggered: false,
            search_input: String::new(),
            search_cursor: 0,
            search_results: Vec::new(),
            selected_index: 0,
            focus: Focus::SearchBar,
            screen: Screen::SearchHome,
            detail_tab: DetailTab::Overview,
            detail_context: None,
            detail_selected: 0,
            detail_scroll: 0,
            trace_chain: None,
            trace_selected: 0,
            trace_scroll: 0,
            exit_confirm_until: None,
            file_count: 0,
            symbol_count: 0,
            edge_count: 0,
            index_mode: "none".into(),
        }
    }

    #[test]
    fn q_is_search_input_not_quit() {
        let mut app = test_app();

        app.handle_key_press(KeyEvent::from(KeyCode::Char('q')));

        assert!(!app.should_quit);
        assert_eq!(app.search_input, "q");
    }

    #[test]
    fn empty_search_requires_second_escape_to_quit() {
        let mut app = test_app();

        app.handle_search_key(KeyCode::Esc);
        assert!(!app.should_quit);
        assert!(app.exit_confirmation_active());

        app.handle_search_key(KeyCode::Esc);
        assert!(app.should_quit);
    }
}
