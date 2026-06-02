//! TUI application state machine and main event loop.
//!
//! Phase 4: search + symbol detail (Overview / Callers / Callees / Source tabs).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use atlas_engine::{CallerChain, ContextView, RawTraceEngine, SearchResult, Store};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Gauge, Paragraph},
    Frame,
};

use super::auto_index::AutoIndexHandle;
use super::event::{Event, EventHandler};
use super::session::GraphSession;
use super::widgets::context_view::DetailTab;
use super::widgets::{context_view, results_list, search_bar, status_bar, trace_view};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

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
    AutoIndexing,
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

    // ── Auto-index (Phase 6) ──────────────────────────────────────────
    auto_index: Option<AutoIndexHandle>,

    // ── DB stats (cached once) ────────────────────────────────────────
    file_count: i64,
    symbol_count: i64,
    edge_count: i64,
}

impl App {
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        let stats = store.get_stats().ok();
        let (file_count, symbol_count, edge_count) = stats
            .map(|s| (s.total_files, s.total_symbols, s.total_edges))
            .unwrap_or_default();

        let session = GraphSession::new(Arc::clone(&store), project_root.clone());

        let (screen, auto_index) = if file_count == 0 {
            let handle =
                super::auto_index::spawn_auto_index(Arc::clone(&store), project_root.clone());
            (Screen::AutoIndexing, Some(handle))
        } else {
            (Screen::SearchHome, None)
        };

        Self {
            should_quit: false,
            store,
            project_root,
            session,
            auto_index,
            search_input: String::new(),
            search_cursor: 0,
            search_results: Vec::new(),
            selected_index: 0,
            focus: Focus::SearchBar,
            screen,
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
        if self.screen != Screen::AutoIndexing {
            return;
        }
        if let Some(ref ai) = self.auto_index {
            if ai.done.load(Ordering::SeqCst) {
                let mut ai_handle = self.auto_index.take().unwrap();
                match ai_handle.take_result() {
                    Some(Ok(())) => {
                        // Refresh DB stats from the now-populated store.
                        if let Ok(stats) = self.store.get_stats() {
                            self.file_count = stats.total_files;
                            self.symbol_count = stats.total_symbols;
                            self.edge_count = stats.total_edges;
                        }
                        // Force-rebuild the graph snapshot so search / context
                        // engines are immediately usable.
                        if let Err(e) = self.session.force_refresh() {
                            tracing::error!("Failed to refresh graph after auto-index: {e}");
                        }
                        self.screen = Screen::SearchHome;
                    }
                    Some(Err(e)) => {
                        tracing::error!("Auto-index failed: {e:?}");
                        // Transition anyway — user sees empty SearchHome.
                        self.screen = Screen::SearchHome;
                    }
                    None => {
                        // Result already taken (should not happen).
                        self.screen = Screen::SearchHome;
                    }
                }
            }
        }
    }

    fn handle_key_press(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }

        if key.code == KeyCode::Char('q')
            && !(self.screen == Screen::SearchHome && self.focus == Focus::SearchBar)
        {
            self.should_quit = true;
            return;
        }

        let code = key.code;
        match self.screen {
            Screen::SearchHome => self.handle_search_key(code),
            Screen::SymbolDetail => self.handle_detail_key(code),
            Screen::TraceView => self.handle_trace_key(code),
            Screen::AutoIndexing => {
                // Auto-index is not interruptible. Ignore all keys —
                // the user must wait for the pipeline to finish.
            }
        }
    }

    // ── SearchHome key handling ───────────────────────────────────────────

    fn handle_search_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                if self.focus == Focus::Results || !self.search_input.is_empty() {
                    self.reset_search_input();
                } else {
                    self.should_quit = true;
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
                // Phase 5: trigger callers trace.
                if let Some(ctx) = &self.detail_context {
                    let trace_engine = RawTraceEngine::new(Arc::clone(self.session.store()));
                    match trace_engine.trace_callers(&ctx.subject.id, 20) {
                        resp if resp.ok => {
                            if let Some(chain) = resp.result {
                                self.trace_chain = Some(chain);
                                self.trace_selected = 0;
                                self.trace_scroll = 0;
                                self.screen = Screen::TraceView;
                            }
                        }
                        _ => {
                            tracing::error!("Trace callers failed for {}", ctx.subject.name);
                        }
                    }
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
            .build_context_for_symbol(&symbol.id)
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
            _ => return,
        };

        if let Some(symbol_id) = target {
            match self
                .session
                .context_builder()
                .build_context_for_symbol(&symbol_id)
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
        let query = self.search_input.clone();
        match self.session.search_engine().search_simple(&query, 100) {
            Ok(results) => {
                self.search_results = results;
                self.selected_index = 0;
                self.focus = Focus::Results;
            }
            Err(e) => {
                tracing::error!("Search failed: {e}");
                self.search_results.clear();
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

        // Body content.
        if self.screen == Screen::AutoIndexing {
            render_auto_index_progress(frame, body_area, &self.auto_index);
        } else {
            // Right panel.
            match (&self.screen, &self.detail_context, &self.trace_chain) {
                (Screen::SymbolDetail, Some(ctx), _) => {
                    // Clamp scroll before render so selected stays visible.
                    let available_height = body_cols[1].height.saturating_sub(2) as usize;
                    if self.detail_selected < self.detail_scroll {
                        self.detail_scroll = self.detail_selected;
                    } else if self.detail_selected >= self.detail_scroll + available_height {
                        self.detail_scroll =
                            self.detail_selected.saturating_sub(available_height - 1);
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
                    frame.render_widget(p, body_cols[1]);
                }
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
            "",
        );
    }
}

// ── helpers ───────────────────────────────────────────────────────────────

/// Render the auto-index progress screen.
fn render_auto_index_progress(frame: &mut Frame, area: Rect, handle: &Option<AutoIndexHandle>) {
    let progress = handle.as_ref().map(|h| h.progress.lock().unwrap().clone());

    let (phase, current, total, message) = match &progress {
        Some(p) => (p.phase.clone(), p.current, p.total, p.message.clone()),
        None => ("Initializing".into(), 0, 0, String::new()),
    };

    // Vertical layout: title, gap, phase label, gauge, message, gap.
    let inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // title
            Constraint::Length(1), // gap
            Constraint::Length(2), // phase label
            Constraint::Length(1), // gauge
            Constraint::Length(1), // message
            Constraint::Length(1), // subtitle
            Constraint::Min(0),    // fill
        ])
        .split(centered_in(area, 64, 8));

    // Title
    let title = Paragraph::new("Auto-indexing project...")
        .style(Style::default().fg(Color::Cyan))
        .alignment(Alignment::Center);
    frame.render_widget(title, inner[0]);

    // Phase label
    let phase_text = format!(
        "  {:<14} {}/{}",
        phase,
        current,
        if total > 0 {
            total.to_string()
        } else {
            "?".to_string()
        }
    );
    let phase_para = Paragraph::new(phase_text).style(Style::default().fg(Color::White));
    frame.render_widget(phase_para, inner[2]);

    // Gauge progress bar
    let ratio = if total > 0 {
        (current as f64 / total as f64).min(1.0)
    } else {
        0.0
    };
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray))
        .ratio(ratio);
    frame.render_widget(gauge, inner[3]);

    // Message
    if !message.is_empty() {
        let msg = Paragraph::new(message).style(Style::default().fg(Color::Gray));
        frame.render_widget(msg, inner[4]);
    }

    // Subtitle
    let subtitle = Paragraph::new("Building initial knowledge graph...")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    frame.render_widget(subtitle, inner[5]);
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
