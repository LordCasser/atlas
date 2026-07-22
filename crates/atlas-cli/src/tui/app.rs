//! Interactive code-graph workbench state and event loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use atlas_engine::{
    CallerChain, ContextView, QueryNeed, SearchResult, Store, has_finalized_repo_cache_for,
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::command_palette::{self, PaletteState};
use super::event::{Event, EventHandler};
use super::jobs::{JobManager, JobResult, TuiJob};
use super::search_session::{ParsedSearch, parse_query};
use super::session::GraphSession;
use super::tool_result::{self, ToolResultView};
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

    // Human-oriented projection of the latest shared MCP handler output.
    last_tool_result: Option<ToolResultView>,

    exit_confirm_until: Option<Instant>,
    /// Whether to show the one-shot help overlay (toggled by '?').
    help_visible: bool,
    palette_visible: bool,
    palette: PaletteState,
    palette_error: Option<String>,
    tool_name: Option<String>,
    tool_scroll: u16,
    tool_raw: bool,
    latest_query_id: Option<String>,

    // ── Job system ────────────────────────────────────────────────────
    job_manager: JobManager,
    /// Stashed parsed search for re-submission after lazy structural.
    pending_search: Option<ParsedSearch>,
    /// Whether lazy structural has been triggered for the current search.
    search_lazy_triggered: bool,
    /// Symbol waiting for the first graph snapshot to finish loading.
    pending_detail_symbol: Option<atlas_engine::SymbolId>,

    // ── DB stats (cached once) ────────────────────────────────────────
    file_count: i64,
    symbol_count: i64,
    edge_count: i64,
    catalog_tier: String,
}

impl App {
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        let stats = store.get_stats().ok();
        let (file_count, symbol_count, edge_count) = stats
            .map(|s| (s.total_files, s.total_symbols, s.total_edges))
            .unwrap_or_default();
        let catalog_tier = detect_catalog_tier(&store);

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
            pending_detail_symbol: None,
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
            catalog_tier,
            last_tool_result: None,
            help_visible: false,
            palette_visible: false,
            palette: PaletteState::default(),
            palette_error: None,
            tool_name: None,
            tool_scroll: 0,
            tool_raw: false,
            latest_query_id: None,
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
                self.pending_detail_symbol = None;
            }
            _ => {}
        }
    }

    fn handle_key_press(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }

        if self.palette_visible {
            self.handle_palette_key(key.code);
            return;
        }

        if key.code == KeyCode::Char(':')
            && (self.focus != Focus::SearchBar || self.search_input.is_empty())
        {
            self.palette_visible = true;
            self.palette.clear();
            self.palette_error = None;
            return;
        }

        if self.last_tool_result.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('x') => {
                    self.last_tool_result = None;
                    self.tool_name = None;
                    self.tool_scroll = 0;
                    self.tool_raw = false;
                    return;
                }
                KeyCode::Char('r') => {
                    self.tool_raw = !self.tool_raw;
                    self.tool_scroll = 0;
                    return;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.tool_scroll = self.tool_scroll.saturating_add(1);
                    return;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.tool_scroll = self.tool_scroll.saturating_sub(1);
                    return;
                }
                KeyCode::PageDown => {
                    self.tool_scroll = self.tool_scroll.saturating_add(10);
                    return;
                }
                KeyCode::PageUp => {
                    self.tool_scroll = self.tool_scroll.saturating_sub(10);
                    return;
                }
                _ => {}
            }
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

    fn handle_palette_key(&mut self, code: KeyCode) {
        if self.palette.form.is_some() {
            self.handle_command_form_key(code);
            return;
        }
        match code {
            KeyCode::Esc => {
                self.palette_visible = false;
                self.palette_error = None;
            }
            KeyCode::Down => {
                let max = self.palette.matches().len().saturating_sub(1);
                self.palette.selected = (self.palette.selected + 1).min(max);
            }
            KeyCode::Up => self.palette.selected = self.palette.selected.saturating_sub(1),
            KeyCode::Tab => self.palette.select_current(),
            KeyCode::Enter => self.open_palette_form(),
            KeyCode::Backspace => {
                if self.palette.cursor > 0 {
                    let byte = crate::tui::byte_index_at_char(
                        &self.palette.input,
                        self.palette.cursor - 1,
                    );
                    self.palette.input.remove(byte);
                    self.palette.cursor -= 1;
                    self.palette.selected = 0;
                }
            }
            KeyCode::Char(c) => {
                let byte = crate::tui::byte_index_at_char(&self.palette.input, self.palette.cursor);
                self.palette.input.insert(byte, c);
                self.palette.cursor += 1;
                self.palette.selected = 0;
                self.palette_error = None;
            }
            KeyCode::Left => self.palette.cursor = self.palette.cursor.saturating_sub(1),
            KeyCode::Right => {
                self.palette.cursor =
                    (self.palette.cursor + 1).min(self.palette.input.chars().count());
            }
            KeyCode::Home => self.palette.cursor = 0,
            KeyCode::End => self.palette.cursor = self.palette.input.chars().count(),
            _ => {}
        }
    }

    fn open_palette_form(&mut self) {
        let matches = self.palette.matches();
        let Some(command) = matches.get(self.palette.selected) else {
            self.palette_error = Some("No matching command".into());
            return;
        };
        let selection = self.current_tool_context();
        let mut form = command_palette::CommandForm::new(command.name, selection);
        form.prefill_query_id(self.latest_query_id.as_deref());
        self.palette.form = Some(form);
        self.palette_error = None;
    }

    fn handle_command_form_key(&mut self, code: KeyCode) {
        let Some(form) = self.palette.form.as_mut() else {
            return;
        };
        if form.editing {
            match code {
                KeyCode::Esc => {
                    form.editing = false;
                    form.edit_buffer.clear();
                    form.edit_cursor = 0;
                }
                KeyCode::Enter => form.commit_edit(),
                KeyCode::Backspace => {
                    if form.edit_cursor > 0 {
                        let byte =
                            crate::tui::byte_index_at_char(&form.edit_buffer, form.edit_cursor - 1);
                        form.edit_buffer.remove(byte);
                        form.edit_cursor -= 1;
                    }
                }
                KeyCode::Left => form.edit_cursor = form.edit_cursor.saturating_sub(1),
                KeyCode::Right => {
                    form.edit_cursor = (form.edit_cursor + 1).min(form.edit_buffer.chars().count());
                }
                KeyCode::Home => form.edit_cursor = 0,
                KeyCode::End => form.edit_cursor = form.edit_buffer.chars().count(),
                KeyCode::Char(c) => {
                    let byte = crate::tui::byte_index_at_char(&form.edit_buffer, form.edit_cursor);
                    form.edit_buffer.insert(byte, c);
                    form.edit_cursor += 1;
                }
                _ => {}
            }
            return;
        }

        match code {
            KeyCode::Esc => self.palette.form = None,
            KeyCode::Down | KeyCode::Tab => form.move_next(),
            KeyCode::Up | KeyCode::BackTab => form.move_previous(),
            KeyCode::Left => form.cycle(false),
            KeyCode::Right | KeyCode::Char(' ') => form.cycle(true),
            KeyCode::Enter if form.selected < form.fields.len() => form.begin_edit(),
            KeyCode::Enter => self.run_palette_form(),
            _ => {}
        }
    }

    fn run_palette_form(&mut self) {
        let Some(form) = self.palette.form.as_mut() else {
            return;
        };
        match form.arguments() {
            Ok(arguments) => {
                let name = form.command.to_string();
                let cancel = Arc::new(AtomicBool::new(false));
                self.tool_name = Some(name.clone());
                self.tool_scroll = 0;
                self.tool_raw = false;
                self.last_tool_result = None;
                self.job_manager.submit(TuiJob::ToolCall {
                    name,
                    arguments,
                    cancel,
                });
                self.palette_visible = false;
                self.palette.clear();
                self.palette_error = None;
            }
            Err(error) => {
                if let Some(index) = form.first_missing_required() {
                    form.selected = index;
                }
                form.error = Some(error);
            }
        }
    }

    fn current_tool_context(&self) -> Option<(&str, Option<&str>)> {
        if let Some(context) = &self.detail_context {
            return Some((
                context.subject.qualified_name.as_str(),
                context.subject_file_path.as_deref(),
            ));
        }
        self.search_results.get(self.selected_index).map(|result| {
            (
                result.symbol.qualified_name.as_str(),
                result.file_path.as_deref(),
            )
        })
    }

    // ── SearchHome key handling ───────────────────────────────────────────

    fn handle_search_key(&mut self, code: KeyCode) {
        if code == KeyCode::Char('?') {
            self.help_visible = !self.help_visible;
            return;
        }
        if self.help_visible {
            self.help_visible = false;
            // dismiss and process the incoming key
        }

        let has_results = !self.search_results.is_empty();
        let has_tool = self.last_tool_result.is_some();

        // Browsing shortcuts take precedence once a result list exists. Search
        // input remains ordinary text when the search bar owns focus.
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

            KeyCode::Char('/') => {
                self.focus = Focus::SearchBar;
            }

            // Result navigation and common analysis shortcuts.
            KeyCode::Down => {
                self.focus = Focus::Results;
                if has_results {
                    self.selected_index =
                        (self.selected_index + 1).min(self.search_results.len() - 1);
                }
            }
            KeyCode::Char('j') if has_results => {
                self.focus = Focus::Results;
                self.selected_index = (self.selected_index + 1).min(self.search_results.len() - 1);
            }
            KeyCode::Up => {
                self.focus = Focus::Results;
                self.selected_index = self.selected_index.saturating_sub(1);
            }
            KeyCode::Char('k') if has_results => {
                self.focus = Focus::Results;
                self.selected_index = self.selected_index.saturating_sub(1);
            }
            KeyCode::PageDown => {
                self.focus = Focus::Results;
                if has_results {
                    self.selected_index =
                        (self.selected_index + 10).min(self.search_results.len() - 1);
                }
            }
            KeyCode::PageUp => {
                self.focus = Focus::Results;
                self.selected_index = self.selected_index.saturating_sub(10);
            }

            KeyCode::Enter if has_results => {
                self.open_symbol_detail();
            }
            KeyCode::Char('i') if has_results => {
                let symbol = &self.search_results[self.selected_index].symbol;
                let cancel = Arc::new(AtomicBool::new(false));
                self.tool_name = Some("impact".into());
                self.job_manager.submit(TuiJob::ToolCall {
                    name: "impact".into(),
                    arguments: serde_json::json!({"symbol": symbol.qualified_name, "depth": 3}),
                    cancel,
                });
                self.focus = Focus::Results;
            }
            KeyCode::Char('v') if has_results => {
                self.palette_visible = true;
                self.palette.input = "trace ".into();
                self.palette.cursor = self.palette.input.len();
            }
            KeyCode::Char('x') if has_tool => {
                self.last_tool_result = None;
                self.focus = Focus::Results;
            }

            KeyCode::Enter if self.focus == Focus::SearchBar => self.perform_search(),
            KeyCode::Char(c) if self.focus == Focus::SearchBar => {
                search_bar::handle_key(&mut self.search_input, &mut self.search_cursor, c);
            }
            KeyCode::Backspace if self.focus == Focus::SearchBar => {
                if self.search_cursor > 0 {
                    let bp =
                        crate::tui::byte_index_at_char(&self.search_input, self.search_cursor - 1);
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

            _ => {}
        }
    }

    // ── TraceView key handling ──────────────────────────────────────────────

    fn handle_trace_key(&mut self, code: KeyCode) {
        if code == KeyCode::Char('?') {
            self.help_visible = !self.help_visible;
            return;
        }
        if self.help_visible {
            self.help_visible = false;
        }
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
                self.last_tool_result = None;
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
        if code == KeyCode::Char('?') {
            self.help_visible = !self.help_visible;
            return;
        }
        if self.help_visible {
            self.help_visible = false;
        }
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
                        symbol_id: ctx.subject.id,
                        depth: 20,
                        cancel,
                    });
                }
            }
            KeyCode::Char('i') => {
                if let Some(ctx) = &self.detail_context {
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.tool_name = Some("impact".into());
                    self.job_manager.submit(TuiJob::ToolCall {
                        name: "impact".into(),
                        arguments: serde_json::json!({"symbol": ctx.subject.qualified_name, "depth": 3}),
                        cancel,
                    });
                }
            }
            KeyCode::Char('v') => {
                self.palette_visible = true;
                self.palette.input = "trace ".into();
                self.palette.cursor = self.palette.input.len();
            }
            KeyCode::Char('x') if self.last_tool_result.is_some() => {
                // Clear tool result (switch back from tool mode)
                self.last_tool_result = None;
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
        if !has_finalized_repo_cache_for(&self.store, QueryNeed::CallGraph) {
            // A native ContextBuilder view has no coverage envelope. Route
            // partial/scoped/manifest catalogs through the shared MCP handler
            // so Focus, pending work, and known gaps remain visible.
            let result = &self.search_results[self.selected_index];
            let symbol = serde_json::json!({
                "qualified_name": result.symbol.qualified_name,
                "file_path": result.file_path,
                "line": result.symbol.range.start_line.saturating_add(1),
                "kind": result.symbol.kind.as_str(),
                "language": result.symbol.language.as_str(),
            });
            self.tool_name = Some("symbol".into());
            self.tool_scroll = 0;
            self.tool_raw = false;
            self.last_tool_result = None;
            self.job_manager.submit(TuiJob::ToolCall {
                name: "symbol".into(),
                arguments: serde_json::json!({
                    "symbol": symbol,
                    "view": "context",
                    "includeCode": true,
                }),
                cancel: Arc::new(AtomicBool::new(false)),
            });
            self.focus = Focus::Results;
            return;
        }
        let symbol_id = self.search_results[self.selected_index].symbol.id;

        if !self.session.is_initialized() || self.session.needs_refresh() {
            self.pending_detail_symbol = Some(symbol_id);
            self.job_manager.submit(TuiJob::LoadGraph {
                cancel: Arc::new(AtomicBool::new(false)),
            });
            return;
        }

        self.show_symbol_detail(symbol_id);
    }

    fn show_symbol_detail(&mut self, symbol_id: atlas_engine::SymbolId) {
        match self
            .session
            .context_builder()
            .build_context_for_symbol(&symbol_id, true)
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
        self.last_tool_result = None;
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
        self.last_tool_result = None;
    }

    // ── search ────────────────────────────────────────────────────────────

    fn perform_search(&mut self) {
        if self.search_input.trim().is_empty() {
            self.search_results.clear();
            self.selected_index = 0;
            return;
        }
        if self.session.is_initialized() {
            self.job_manager
                .set_graph(self.session.graph_engine().clone());
        }

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
                self.last_tool_result = None;
                tracing::info!("Search returned {count} results");
            }
            JobResult::GraphLoaded(loaded) => match loaded {
                Ok(graph) => {
                    self.session.install_graph(Arc::clone(&graph));
                    self.job_manager.set_graph(graph);
                    if let Some(symbol_id) = self.pending_detail_symbol.take() {
                        self.show_symbol_detail(symbol_id);
                    }
                }
                Err(error) => {
                    self.pending_detail_symbol = None;
                    self.tool_name = Some("Graph".into());
                    self.last_tool_result = Some(ToolResultView::from_text(
                        format!("Failed to load graph: {error}"),
                        true,
                    ));
                    self.tool_scroll = 0;
                    self.tool_raw = false;
                    tracing::error!("Failed to load graph: {error}");
                }
            },
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
                    // 列表仍空 → 保持 Querying（或在 LazyComplete 后会到 Browsing）
                }
            }
            JobResult::LazyComplete {
                files_built,
                files_cached,
            } => {
                tracing::info!("Lazy structural: {files_built} built, {files_cached} cached");
                // Search reads the store directly, so do not rebuild a large
                // graph snapshot on the UI thread. Mark it stale for the next
                // graph-backed action and immediately repeat the search.
                self.session.mark_stale();
                self.refresh_cached_status();
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
                self.trace_chain = chain.map(|chain| *chain);
                self.trace_selected = 0;
                self.trace_scroll = 0;
                self.screen = Screen::TraceView;
            }
            JobResult::ToolOutput {
                name,
                text,
                is_error,
            } => {
                // Shared MCP handlers may have materialized Focus facts. The
                // native TUI graph is a separate snapshot, so invalidate it
                // and refresh the shared Store-derived status boundary after
                // every tool call.
                self.session.mark_stale();
                self.refresh_cached_status();
                let view = ToolResultView::from_text(text, is_error);
                if let Some(query_id) = view.query_id() {
                    self.latest_query_id = Some(query_id.to_owned());
                }
                self.tool_name = Some(name);
                self.last_tool_result = Some(view);
                self.tool_scroll = 0;
                self.tool_raw = false;
            }
        }
    }

    // ── render ────────────────────────────────────────────────────────────

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        search_bar::render(
            frame,
            rows[0],
            &self.search_input,
            self.search_cursor,
            self.focus == Focus::SearchBar,
        );

        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(rows[1]);
        if let Some(output) = &self.last_tool_result {
            tool_result::render(
                frame,
                rows[1],
                self.tool_name.as_deref().unwrap_or("analysis"),
                output,
                self.tool_raw,
                self.tool_scroll,
            );
        } else {
            let result_rows: Vec<results_list::ResultRow> = self
                .search_results
                .iter()
                .cloned()
                .map(Into::into)
                .collect();
            let mut list_scroll = 0;
            results_list::render(
                frame,
                columns[0],
                &result_rows,
                self.selected_index,
                &mut list_scroll,
            );
            match (&self.screen, &self.detail_context, &self.trace_chain) {
                (Screen::SymbolDetail, Some(context), _) => context_view::render(
                    frame,
                    columns[1],
                    context,
                    self.detail_tab,
                    self.detail_selected,
                    self.detail_scroll,
                ),
                (Screen::TraceView, _, Some(chain)) => trace_view::render(
                    frame,
                    columns[1],
                    chain,
                    self.trace_selected,
                    self.trace_scroll,
                ),
                _ => {
                    let lines = if self.job_manager.is_running() {
                        "Running analysis...\n\nEsc cancels the current task"
                    } else if self.search_results.is_empty() {
                        "Search the code graph\n\nType a symbol and press Enter.\nPress : for analysis commands."
                    } else {
                        "Enter  Open symbol\n:      Analysis commands\ni      Impact\nv      Trace commands\n?      Keyboard help"
                    };
                    frame.render_widget(
                        Paragraph::new(lines)
                            .block(Block::default().borders(Borders::ALL).title(" Atlas "))
                            .style(Style::default().fg(Color::DarkGray)),
                        columns[1],
                    );
                }
            }
        }

        let mode = if self.job_manager.is_running() {
            "running"
        } else {
            "ready"
        };
        status_bar::render(
            frame,
            rows[2],
            (self.file_count, self.symbol_count, self.edge_count),
            self.session.is_initialized(),
            &self.catalog_tier,
            mode,
        );

        if self.exit_confirmation_active() {
            render_exit_confirmation(frame, area);
        } else if self.help_visible {
            render_help_popup(frame, area);
        }
        if self.palette_visible {
            command_palette::render(frame, area, &self.palette, self.palette_error.as_deref());
        }
    }

    fn refresh_cached_status(&mut self) {
        if let Ok(stats) = self.store.get_stats() {
            self.file_count = stats.total_files;
            self.symbol_count = stats.total_symbols;
            self.edge_count = stats.total_edges;
            self.catalog_tier = detect_catalog_tier(&self.store);
        }
    }
}

/// Detect index mode by delegating to the canonical `Store::read_catalog_tier()`.
///
/// This is the single source of truth for index-mode detection, shared by
/// CLI, MCP, and TUI.  Previously the TUI maintained its own divergent
/// detection logic; see issue 2.1 in the pre-release review.
fn detect_catalog_tier(store: &Store) -> String {
    store
        .read_catalog_tier()
        .unwrap_or_else(|_| "unknown".into())
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

fn render_help_popup(frame: &mut Frame, area: Rect) {
    let popup = centered_in(area, 58, 18);
    frame.render_widget(Clear, popup);

    let lines = [
        "  /           Focus search",
        "  :           Open analysis command palette",
        "  Enter       Open selected symbol / run command",
        "  Tab         Next detail tab / complete palette command",
        "  j k / arrows  Move selection or scroll output",
        "  PgUp PgDn   Move by page",
        "  i           Run impact for selected symbol",
        "  t           Trace callers from symbol detail",
        "  v           Open palette with trace selected",
        "  r           Toggle facts / raw tool response",
        "  x / Esc     Close analysis output / go back",
        "  ?           Toggle this help",
        "  Ctrl-C      Exit immediately",
        "",
        "  Commands open parameter forms; no JSON is required.",
        "  In forms: Enter edit/run, Tab move, Left/Right choose.",
    ];
    let p = Paragraph::new(lines.join("\n"))
        .block(Block::default().borders(Borders::ALL).title(" Help "))
        .style(Style::default().fg(Color::White));
    frame.render_widget(p, popup);
}

fn centered_in(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let store = Store::open_in_memory().expect("in-memory store");
        store.init_schema().expect("initialize in-memory schema");
        let store = Arc::new(store);
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
            pending_detail_symbol: None,
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
            catalog_tier: "none".into(),
            last_tool_result: None,
            help_visible: false,
            palette_visible: false,
            palette: PaletteState::default(),
            palette_error: None,
            tool_name: None,
            tool_scroll: 0,
            tool_raw: false,
            latest_query_id: None,
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

    #[test]
    fn colon_opens_command_palette() {
        let mut app = test_app();
        app.handle_key_press(KeyEvent::from(KeyCode::Char(':')));
        assert!(app.palette_visible);
    }

    #[test]
    fn colon_remains_available_in_search_filters() {
        let mut app = test_app();
        for c in "lang:rust".chars() {
            app.handle_key_press(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(app.search_input, "lang:rust");
        assert!(!app.palette_visible);
    }

    #[test]
    fn perform_search_does_not_build_graph_on_ui_thread() {
        let mut app = test_app();
        app.search_input = "missing_symbol".into();

        app.perform_search();

        assert!(!app.session.is_initialized());
        assert!(app.job_manager.is_running());
    }

    #[test]
    fn graph_load_failure_is_visible_in_the_workbench() {
        let mut app = test_app();

        app.handle_job_completion(JobResult::GraphLoaded(Err("broken graph".into())));

        assert_eq!(app.tool_name.as_deref(), Some("Graph"));
        assert!(app.last_tool_result.is_some());
    }

    #[test]
    fn partial_catalog_symbol_detail_uses_shared_focus_aware_handler() {
        use atlas_engine::{
            FileId, FileInfo, GraphEngine, Language, ParseStatus, SearchEngine, SearchOptions,
            SymbolDef, SymbolId, SymbolKind, TextRange,
        };

        let mut app = test_app();
        let file_id = FileId::generate("a.ts");
        app.store
            .upsert_file(&FileInfo {
                file_id,
                path: "a.ts".into(),
                language: Language::TypeScript,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        app.store
            .upsert_file_extraction_state(
                &file_id,
                "manifest",
                "hash",
                "complete",
                atlas_engine::FactCoverage::default(),
            )
            .unwrap();
        let symbol = SymbolDef {
            id: SymbolId::generate(
                &file_id,
                Language::TypeScript.as_str(),
                "handler",
                SymbolKind::Function.as_str(),
                None,
            ),
            kind: SymbolKind::Function,
            name: "handler".into(),
            qualified_name: "handler".into(),
            symbol_path: vec!["handler".into()],
            file_id,
            language: Language::TypeScript,
            range: TextRange::default(),
            name_range: TextRange::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "manifest".into(),
        };
        app.store.insert_symbols(&[symbol]).unwrap();
        let search = SearchEngine::new(Arc::clone(&app.store), Arc::new(GraphEngine::empty()));
        app.search_results = search
            .search("handler", 10, &SearchOptions::default())
            .unwrap();

        app.open_symbol_detail();

        assert_eq!(app.tool_name.as_deref(), Some("symbol"));
        assert!(app.pending_detail_symbol.is_none());
        assert!(!app.session.is_initialized());
    }

    #[test]
    fn tool_output_invalidates_native_graph_and_refreshes_catalog_status() {
        use atlas_engine::{FactCoverage, FileId, FileInfo, Language, ParseStatus};

        let mut app = test_app();
        let file_id = FileId::generate("a.ts");
        app.store
            .upsert_file(&FileInfo {
                file_id,
                path: "a.ts".into(),
                language: Language::TypeScript,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        app.store
            .upsert_file_extraction_state(
                &file_id,
                "structural",
                "hash",
                "complete",
                FactCoverage::default(),
            )
            .unwrap();

        app.handle_job_completion(JobResult::ToolOutput {
            name: "search".into(),
            text: r#"{"ok":true}"#.into(),
            is_error: false,
        });

        assert!(app.session.needs_refresh());
        assert_eq!(app.file_count, 1);
        assert_eq!(app.catalog_tier, "structural");
    }

    #[test]
    fn command_palette_renders_in_narrow_terminal() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut app = test_app();
        app.palette_visible = true;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Command Palette"));
        assert!(rendered.contains("impact"));
    }

    #[test]
    fn help_lists_result_view_toggle_at_minimum_terminal_size() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut app = test_app();
        app.help_visible = true;
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Toggle facts / raw"));
        assert!(rendered.contains("Ctrl-C"));
    }

    #[test]
    fn tool_output_uses_full_body_width() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut app = test_app();
        app.tool_name = Some("impact".into());
        app.last_tool_result = Some(ToolResultView::from_text(
            "{\n  \"ok\": true\n}".into(),
            false,
        ));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        let row = (0..80)
            .map(|x| terminal.backend().buffer()[(x, 3)].symbol())
            .collect::<String>();
        assert!(row.starts_with("┌ Impact"), "unexpected row: {row}");
        assert!(row.contains("Complete"));
        assert!(row.ends_with('┐'));
    }

    #[test]
    fn r_toggles_raw_tool_response_and_resets_scroll() {
        let mut app = test_app();
        app.last_tool_result = Some(ToolResultView::from_text(r#"{"result":1}"#.into(), false));
        app.tool_scroll = 8;

        app.handle_key_press(KeyEvent::from(KeyCode::Char('r')));

        assert!(app.tool_raw);
        assert_eq!(app.tool_scroll, 0);
    }

    #[test]
    fn query_id_from_tool_output_prefills_resume_form() {
        let mut app = test_app();
        app.handle_job_completion(JobResult::ToolOutput {
            name: "impact".into(),
            text: r#"{"query_id":"q_123","result":{}}"#.into(),
            is_error: false,
        });
        app.palette.input = "resume_query".into();
        app.open_palette_form();

        let form = app.palette.form.as_ref().unwrap();
        assert_eq!(form.arguments().unwrap()["query_id"], "q_123");
    }
}
