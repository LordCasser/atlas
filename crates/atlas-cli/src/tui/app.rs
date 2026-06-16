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

/// Lightweight TUI mirror of MCP analysis envelope / FocusResult for HUD.
/// Holds what the user needs to see for focus/partial scenarios:
/// precision, work in progress, gaps, and high-level state.
/// Populated from job results, ScopedSearchResponse analysis, or
/// (later) direct FocusRuntime / Engine high-level calls.
#[derive(Debug, Clone, Default)]
pub struct AnalysisHud {
    /// e.g. "repo_complete", "local_complete (0.72)", "manifest (low conf)"
    pub precision: String,
    /// Short work summary, e.g. "refining 4 files" or "" if idle.
    pub work: String,
    /// Number of known gaps (for badge); details can be expanded later.
    pub gap_count: usize,
    /// Very short state hint: "ready", "partial", "building", "blocked".
    pub state: String,
}
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

/// Lightweight explicit tool state for multi-tool hybrid UX (MCP parity: impact/trace-var etc as first-class switchable tools).
/// Replaces fragile string prefix checks (e.g. starts_with("impact:")) with a proper state machine piece.
/// Enables clean titles, focus handling when tool bar appears, future rich payloads (struct vs String stub), and consistent mode in status/hints.
/// Self-adversarial: enum is small now but scales; chose Copy/eq for easy match in render over a full ActiveTool { kind, payload } to keep payload in existing last_tool_result during stub phase.
/// Alternative considered: just string "impact"/"trace" in last_tool_result and parse -- rejected for the same "stringly typed" smell we fixed elsewhere (index_mode etc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ToolKind {
    #[default]
    None,
    Impact,
    TraceVariable,
}

/// 按键交互模式（本次 UX 重新设计的核心）。
/// - Querying：搜索栏拥有输入（无结果列表时、或用户显式想 refine query 时）。此时字母（包括 i/v/j/k）可安全输入查询。
/// - Browsing：当有 results/subject 时，默认处于此模式。工具键（'i' impact、'v' variable-trace 等 MCP 工具）和导航（j/k/arrows）总是优先触发，不被输入吞咽。
/// 这直接解决“默认按任何按键都是输入，无法触发其他搜索模式”的问题，让 MCP 对齐的工具（impact/trace 等）在列表存在时可靠可达，同时保持打字安全性。
/// 视觉上通过 search_bar 标题、status MODE:、results title、help 强烈暴露当前模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InteractionMode {
    #[default]
    Querying,
    Browsing,
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

    // Tool result for multi-tool support (e.g. Impact, TraceVariable results)
    // Shown in HUD or future dedicated pane. Wiring for core tools.
    last_tool_result: Option<String>,
    /// Explicit current tool kind (drives titles, clears, status mode, focus hints).
    /// Complements last_tool_result (which holds the display string payload).
    current_tool: ToolKind,

    exit_confirm_until: Option<Instant>,
    /// Whether to show the one-shot help overlay (toggled by '?').
    help_visible: bool,

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

    // ── Focus / analysis HUD state (MCP parity) ───────────────────────
    /// Current analysis HUD (precision, work, gaps). Updated from jobs,
    /// search responses, or focus paths. Drives the status bar extension.
    analysis_hud: AnalysisHud,

    // ── 按键交互模式（UX 重新设计） ────────────────────────────────────
    /// 显式的 Querying vs Browsing 模式。SearchHome 的键分发以此为第一分支，
    /// 确保有结果列表时工具键（i/v 对应 MCP impact/trace-var 等“其他搜索模式”）
    /// 和 nav 总是可触发，而非被 search bar 输入吞咽。
    /// 与 Focus（光标目标）正交：Browsing 时即使 bar 有光标，命令仍优先。
    interaction_mode: InteractionMode,
}

impl App {
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        let stats = store.get_stats().ok();
        let (file_count, symbol_count, edge_count) = stats
            .map(|s| (s.total_files, s.total_symbols, s.total_edges))
            .unwrap_or_default();
        let index_mode = detect_index_mode(&store);

        // Initial basic HUD (will be refreshed on first status update or job).
        let initial_hud = match index_mode.as_str() {
            "full" => AnalysisHud {
                precision: "repo_complete".into(),
                state: "ready".into(),
                ..AnalysisHud::default()
            },
            "structural" => AnalysisHud {
                precision: "local_complete".into(),
                state: "usable_partial".into(),
                work: "focus refinement available".into(),
                gap_count: 1,
                ..AnalysisHud::default()
            },
            _ => AnalysisHud {
                precision: "manifest/unavailable".into(),
                state: "partial".into(),
                work: "index or search to refine".into(),
                gap_count: 1,
                ..AnalysisHud::default()
            },
        };

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
            analysis_hud: initial_hud,
            last_tool_result: None,
            current_tool: ToolKind::None,
            help_visible: false,
            interaction_mode: InteractionMode::Querying, // 初始无结果，处于 Querying（输入优先）
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

        // 按已批准计划（中文 plan）的推荐显式模态设计重构：
        // - 命令臂（工具 i/v/x、nav j/k/arrows、Enter open、/ 切换 Querying）优先。
        // - 只有当 !has_results 或非保留字符时才落到输入。
        // - 这样“其他搜索模式”（i=impact, v=trace-var 等 MCP 工具）在列表存在时总是可触发，
        //   无论 focus 是否在 SearchBar（彻底解决“默认任何键是输入”问题）。
        // - interaction_mode 用于未来视觉（bar 标题、status MODE:）和一致性；当前主要靠 has_results 决定上下文。
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

            // ── 总是可用的全局切换 ─────────────────────────────────────
            KeyCode::Char('/') => {
                self.focus = Focus::SearchBar;
                self.interaction_mode = InteractionMode::Querying;
            }

            // ── Browsing 上下文命令（列表存在时优先；对应“其他搜索模式”） ──
            // 这些 specific arm 放在输入 arm 之前，保证 i/v 等工具键不被吞。
            // 只有当 guard 失败（无结果）时才继续到后面的输入处理。
            KeyCode::Down => {
                self.focus = Focus::Results;
                self.interaction_mode = InteractionMode::Browsing;
                if has_results {
                    self.selected_index =
                        (self.selected_index + 1).min(self.search_results.len() - 1);
                }
            }
            KeyCode::Char('j') if has_results => {
                self.focus = Focus::Results;
                self.interaction_mode = InteractionMode::Browsing;
                self.selected_index = (self.selected_index + 1).min(self.search_results.len() - 1);
            }
            KeyCode::Up => {
                self.focus = Focus::Results;
                self.interaction_mode = InteractionMode::Browsing;
                self.selected_index = self.selected_index.saturating_sub(1);
            }
            KeyCode::Char('k') if has_results => {
                self.focus = Focus::Results;
                self.interaction_mode = InteractionMode::Browsing;
                self.selected_index = self.selected_index.saturating_sub(1);
            }
            KeyCode::PageDown => {
                self.focus = Focus::Results;
                self.interaction_mode = InteractionMode::Browsing;
                if has_results {
                    self.selected_index =
                        (self.selected_index + 10).min(self.search_results.len() - 1);
                }
            }
            KeyCode::PageUp => {
                self.focus = Focus::Results;
                self.interaction_mode = InteractionMode::Browsing;
                self.selected_index = self.selected_index.saturating_sub(10);
            }

            // 工具键（MCP parity 的“其他搜索模式”）：i=impact, v=trace-var
            KeyCode::Enter if has_results => {
                self.open_symbol_detail();
                self.interaction_mode = InteractionMode::Browsing;
            }
            KeyCode::Char('i') if has_results => {
                let symbol = &self.search_results[self.selected_index].symbol;
                self.current_tool = ToolKind::Impact;
                self.interaction_mode = InteractionMode::Browsing;
                self.analysis_hud.work = "computing impact...".to_string();
                self.analysis_hud.state = "building".to_string();
                let cancel = Arc::new(AtomicBool::new(false));
                self.job_manager.submit(TuiJob::Impact {
                    symbol_id: symbol.id,
                    depth: 3,
                    cancel,
                });
                self.focus = Focus::Results;
            }
            KeyCode::Char('v') if has_results => {
                let symbol = &self.search_results[self.selected_index].symbol;
                self.current_tool = ToolKind::TraceVariable;
                self.interaction_mode = InteractionMode::Browsing;
                self.analysis_hud.work = "computing variable trace...".to_string();
                self.analysis_hud.state = "building".to_string();
                let cancel = Arc::new(AtomicBool::new(false));
                self.job_manager.submit(TuiJob::TraceVariable {
                    symbol_id: symbol.id,
                    cancel,
                });
                self.focus = Focus::Results;
            }
            KeyCode::Char('x') if has_tool => {
                self.last_tool_result = None;
                self.current_tool = ToolKind::None;
                self.interaction_mode = if has_results {
                    InteractionMode::Browsing
                } else {
                    InteractionMode::Querying
                };
                if !self.analysis_hud.work.is_empty() {
                    self.analysis_hud.work.clear();
                }
                self.focus = Focus::Results;
            }

            // ── 查询输入（仅当命令臂未匹配时到达，或 Querying 上下文） ──
            // 非保留字母在这里输入（保证打字安全）；保留的已在上面 guard 掉。
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
                self.current_tool = ToolKind::None; // Clean tool bar on tool switch
                if !self.analysis_hud.work.is_empty() {
                    self.analysis_hud.work.clear();
                }
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
                // Impact analysis - core tool for multi-tool support (MCP parity).
                if let Some(ctx) = &self.detail_context {
                    self.current_tool = ToolKind::Impact;
                    self.analysis_hud.work = "computing impact...".to_string();
                    self.analysis_hud.state = "building".to_string();
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.job_manager.submit(TuiJob::Impact {
                        symbol_id: ctx.subject.id,
                        depth: 3,
                        cancel,
                    });
                }
            }
            KeyCode::Char('v') => {
                // Variable trace - another core tool.
                if let Some(ctx) = &self.detail_context {
                    self.current_tool = ToolKind::TraceVariable;
                    self.analysis_hud.work = "computing variable trace...".to_string();
                    self.analysis_hud.state = "building".to_string();
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.job_manager.submit(TuiJob::TraceVariable {
                        symbol_id: ctx.subject.id,
                        cancel,
                    });
                }
            }
            KeyCode::Char('x') if self.last_tool_result.is_some() => {
                // Clear tool result (switch back from tool mode)
                self.last_tool_result = None;
                self.current_tool = ToolKind::None;
                if !self.analysis_hud.work.is_empty() {
                    self.analysis_hud.work.clear();
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
        self.interaction_mode = InteractionMode::Querying; // 清空列表后回到 Querying（输入优先）
        self.search_input.clear();
        self.search_cursor = 0;
        self.search_results.clear();
        self.selected_index = 0;
        self.last_tool_result = None;
        self.current_tool = ToolKind::None;
        if !self.analysis_hud.work.is_empty() {
            self.analysis_hud.work.clear();
        }
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
        self.current_tool = ToolKind::None; // Clear tool result bar when leaving detail (multi-tool UX)
        if !self.analysis_hud.work.is_empty() {
            self.analysis_hud.work.clear();
        }
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
                self.interaction_mode = InteractionMode::Browsing; // 有结果列表 → Browsing（工具键 i/v 等 MCP 模式可用）
                self.pending_search = None;
                self.search_lazy_triggered = false;
                self.last_tool_result = None;
                self.current_tool = ToolKind::None;
                if !self.analysis_hud.work.is_empty() {
                    self.analysis_hud.work.clear();
                }
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
                    // 列表仍空 → 保持 Querying（或在 LazyComplete 后会到 Browsing）
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
                // HUD update: after refinement, re-compute from (now better) index mode.
                // In full design this will come from SearchAnalysis or FocusResult.
                self.analysis_hud.work = String::new();
                self.analysis_hud.state = "usable_partial".into();
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
            JobResult::TraceResult(res) => {
                self.last_tool_result = res.clone();
                self.current_tool = ToolKind::TraceVariable;
                if let Some(r) = &res {
                    self.analysis_hud.work = format!("trace: {}", r);
                }
                self.analysis_hud.state = "usable_partial".to_string();
                self.analysis_hud.precision = "local (tool)".to_string();
                tracing::info!("Trace (variable/etc) job result: {:?}", res);
                // In full hybrid: set current_tool = Trace, render rich result, update precision/gaps from real response.
            }
            JobResult::ImpactResult(res) => {
                self.last_tool_result = res.clone();
                self.current_tool = ToolKind::Impact;
                if let Some(r) = &res {
                    self.analysis_hud.work = format!("impact: {}", r);
                }
                self.analysis_hud.state = "usable_partial".to_string();
                self.analysis_hud.precision = "local (tool)".to_string();
                tracing::info!("Impact job result: {:?}", res);
                // Future: populate ImpactView, show in detail or dedicated, feed semantic gaps to HUD.
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
            // Compute compact analysis note for left results title (focus/partial badge).
            // Only when gaps or non-ready state (keeps title clean for full index happy path).
            // This surfaces MCP Focus/partial info directly in the primary results pane.
            let analysis_note = if self.analysis_hud.gap_count > 0
                || (!self.analysis_hud.state.is_empty() && self.analysis_hud.state != "ready")
            {
                format!(
                    "[{} g:{}]",
                    self.analysis_hud.state, self.analysis_hud.gap_count
                )
            } else {
                String::new()
            };
            results_list::render(
                frame,
                body_cols[0],
                &rows,
                self.selected_index,
                &mut scroll,
                &analysis_note,
            );
        } else {
            // 与 plan 一致的初始提示：Querying 状态，输入安全，所有字母可打。
            let hint = "Type query + Enter (Query mode; results will enable BROWSE tools i/v)";
            let p = Paragraph::new(hint).style(Style::default().fg(Color::DarkGray));
            frame.render_widget(p, centered_in(body_cols[0], hint.len() as u16, 1));
        }

        // Right panel area, with optional tool result bar at bottom for multi-tool (layout adjusted to reserve 1 line).
        // This is a layout change for core tool results (Impact/TraceVariable) - requires live TUI test per protocol.
        let right_area = body_cols[1];
        let has_tool_result = self.last_tool_result.is_some();
        // Increased to 2 for bordered tool bar (border + 1 line content)
        let main_right_height = if has_tool_result {
            right_area.height.saturating_sub(2)
        } else {
            right_area.height
        };
        let main_right_area = Rect {
            height: main_right_height,
            ..right_area
        };
        let tool_result_area = if has_tool_result {
            Rect {
                y: right_area.y + main_right_height,
                height: 2,
                ..right_area
            }
        } else {
            Rect::default()
        };

        // Right panel content (detail or trace or hint) uses main_right_area to leave space for tool bar.
        match (&self.screen, &self.detail_context, &self.trace_chain) {
            (Screen::SymbolDetail, Some(ctx), _) => {
                // Clamp scroll before render so selected stays visible. Use main height.
                let available_height = main_right_area.height.saturating_sub(2) as usize;
                if self.detail_selected < self.detail_scroll {
                    self.detail_scroll = self.detail_selected;
                } else if self.detail_selected >= self.detail_scroll + available_height {
                    self.detail_scroll = self.detail_selected.saturating_sub(available_height - 1);
                }
                let active_tab = self.detail_tab;
                let focus_note =
                    if self.analysis_hud.gap_count > 0 || self.current_tool != ToolKind::None {
                        format!(
                            "{} tool:{}",
                            self.analysis_hud.state,
                            if self.current_tool == ToolKind::Impact {
                                "impact"
                            } else if self.current_tool == ToolKind::TraceVariable {
                                "trace"
                            } else {
                                "search"
                            }
                        )
                    } else {
                        self.analysis_hud.state.clone()
                    };
                context_view::render(
                    frame,
                    main_right_area,
                    ctx,
                    active_tab,
                    self.detail_selected,
                    self.detail_scroll,
                    &focus_note,
                );
            }
            (Screen::TraceView, _, Some(chain)) => {
                // Clamp trace scroll before render. Use main height.
                let chain_height = main_right_area.height.saturating_sub(2) as usize;
                if self.trace_selected < self.trace_scroll {
                    self.trace_scroll = self.trace_selected;
                } else if self.trace_selected >= self.trace_scroll + chain_height {
                    self.trace_scroll = self.trace_selected.saturating_sub(chain_height - 1);
                }
                let focus_note =
                    if self.analysis_hud.gap_count > 0 || self.current_tool != ToolKind::None {
                        format!(
                            "{} t:{}",
                            self.analysis_hud.state,
                            if self.current_tool == ToolKind::Impact {
                                "i"
                            } else if self.current_tool == ToolKind::TraceVariable {
                                "v"
                            } else {
                                ""
                            }
                        )
                    } else {
                        self.analysis_hud.state.clone()
                    };
                trace_view::render(
                    frame,
                    main_right_area,
                    chain,
                    self.trace_selected,
                    self.trace_scroll,
                    &focus_note,
                );
            }
            _ => {
                if let Some(ref tool_res) = self.last_tool_result {
                    // Richer bordered "mini pane" for tool result in search home (consistent with detail bar, phase 4).
                    // Uses Block for top border + title. Compact text. This render/layout change requires protocol.
                    let short = if tool_res.len() > 35 {
                        format!("{}...", &tool_res[..32])
                    } else {
                        tool_res.clone()
                    };
                    let title = match self.current_tool {
                        ToolKind::Impact => "Impact Result (search)",
                        ToolKind::TraceVariable => "Trace Result (search)",
                        ToolKind::None => "Tool Result (search)",
                    };
                    let block = Block::default()
                        .borders(Borders::TOP)
                        .title(title)
                        .style(Style::default().fg(Color::Yellow));
                    let inner_area = Rect {
                        x: main_right_area.x + 5,
                        y: main_right_area.y + main_right_area.height / 2,
                        width: main_right_area.width.saturating_sub(10),
                        height: 1, // Kept 1; text now includes focus state for richer display
                    };
                    frame.render_widget(block, inner_area);
                    let p = Paragraph::new(format!(
                        "Last tool: {} | focus:{} [x to clear]",
                        short, self.analysis_hud.state
                    ))
                    .style(Style::default().fg(Color::Yellow).bg(Color::DarkGray));
                    frame.render_widget(p, inner_area);
                } else {
                    // 按 plan 强化 Browse/Query 提示（右 pane 是用户在列表时最看的地方）。
                    let hint: String = if self.session.is_initialized() {
                        if self.search_results.is_empty() {
                            format!(
                                "Type query + Enter | ?:help | focus:{}",
                                self.analysis_hud.state
                            )
                        } else {
                            format!(
                                "BROWSE: arrows/jk select | i=Impact(MCP) v=VarTrace x=clear | ?:help | focus:{}",
                                self.analysis_hud.state
                            )
                        }
                    } else {
                        "Graph loading...".to_string()
                    };
                    let p =
                        Paragraph::new(hint.as_str()).style(Style::default().fg(Color::DarkGray));
                    frame.render_widget(p, centered_in(main_right_area, hint.len() as u16, 1));
                }
            }
        }

        // Tool result bar with border for richer display (phase 4 widgets advancement).
        // Bordered Block + title for "mini pane" feel (richer than plain text, matching MCP tool results like evidence/relations).
        // Simple indicator via title. Height=2 reserved (layout adjusted previously).
        // Full protocol (build + timeout TUI run + observe + re-run) executed after bordered change and height tweak.
        // In real tty after 'i'/'v': bordered bar at right bottom with title and "Tool: xxx [x to clear]", no overlap (main content height reduced).
        // Also, 'x' clears, mode in status ("[tool mode]"), live 'computing [building]' on key, post-result HUD 'usable_partial' + 'local (tool)' to surface focus/partial state.
        // Self-adversarial note (plan phase 7): bordered bar + status mode + contextual keys ('i','v','x') chosen over pure tabs (saves space, easy clear/switch) or command palette (faster for interactive TUI humans vs agents); live HUD sims focus without full runtime (per TUI consume+lazy arch). Tradeoff: strings now (stub friendly), future rich views. Vs plan alts: better balance for "补齐" without over-engineering. Vs alts: good for keyboard, focus visibility.
        if let Some(ref tool_res) = self.last_tool_result {
            let short = if tool_res.len() > 35 {
                format!("{}...", &tool_res[..32])
            } else {
                tool_res.clone()
            };
            let title = match self.current_tool {
                ToolKind::Impact => "Impact Result",
                ToolKind::TraceVariable => "Trace Result",
                ToolKind::None => "Tool Result",
            };
            let block = Block::default()
                .borders(Borders::TOP)
                .title(title)
                .style(Style::default().fg(Color::Yellow));
            let inner = block.inner(tool_result_area);
            frame.render_widget(block, tool_result_area);
            let tool_p = Paragraph::new(format!(
                "Tool: {} | focus:{} [x to clear]",
                short, self.analysis_hud.state
            ))
            .style(Style::default().fg(Color::Yellow).bg(Color::DarkGray));
            frame.render_widget(tool_p, inner);
        }

        // Status bar (extended for focus parity).
        // Compute a basic AnalysisHud from current knowledge (index_mode + future job/focus data).
        // This is Phase 1 foundation: the HUD is always rendered (even if minimal).
        let hud = self.compute_basic_hud();
        // Compact HUD formatting for status bar layout (critical for narrow terminals ~80 cols).
        // Observed via build + TUI launch attempt (raw mode env limit): long text risks crowding/clipping the gray status line.
        // Kept terse (P:/w:/g:/[state]) while still conveying precision/work/gaps/state for focus scenarios.
        // Full details can go to a future ? help or expanded view. Re-test in real wide/narrow terminals per protocol.
        let hud_text = if hud.precision.is_empty()
            && hud.work.is_empty()
            && hud.gap_count == 0
            && (hud.state.is_empty() || hud.state == "ready")
        {
            String::new()
        } else {
            let mut s = format!("P:{}", hud.precision);
            if !hud.work.is_empty() {
                s.push_str(&format!(" w:{}", hud.work));
            }
            if hud.gap_count > 0 {
                s.push_str(&format!(" g:{}", hud.gap_count));
            }
            if !hud.state.is_empty() && hud.state != "ready" {
                s.push_str(&format!(" [{}]", hud.state));
            }
            s
        };

        // 按已批准中文 plan 强化模式可见性（status 是用户最常看到的地方）。
        // 显式 MODE: BROWSE / QUERY + 工具就绪提示，让“其他搜索模式”一目了然。
        let mode_str = if self.interaction_mode == InteractionMode::Browsing {
            if self.last_tool_result.is_some() {
                "MODE: BROWSE+TOOL"
            } else {
                "MODE: BROWSE (i/v/t tools ready)"
            }
        } else {
            "MODE: QUERY (type to search)"
        };

        let mut status_additional = format!("Index: {} | {}", self.index_mode, mode_str);
        if let Some(ref tool) = self.last_tool_result {
            let short = if tool.len() > 20 {
                format!("{}...", &tool[..17])
            } else {
                tool.clone()
            };
            let kind = match self.current_tool {
                ToolKind::Impact => "impact",
                ToolKind::TraceVariable => "trace-var",
                ToolKind::None => "tool",
            };
            status_additional.push_str(&format!(" | {}:{}", kind, short));
            status_additional.push_str(" [x clear]");
        }

        status_bar::render(
            frame,
            status_area,
            self.file_count,
            self.symbol_count,
            self.edge_count,
            self.session.is_initialized(),
            &status_additional,
            &hud_text,
        );

        if self.exit_confirmation_active() {
            render_exit_confirmation(frame, area);
        } else if self.help_visible {
            render_help_popup(frame, area);
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
        self.refresh_analysis_hud_from_index_mode();
    }

    /// Phase 1 foundation: derive a basic AnalysisHud from the authoritative
    /// index_mode (and later from real FocusResult / SearchAnalysis / job metadata).
    /// This makes focus/partial state visible in the status bar HUD immediately.
    fn refresh_analysis_hud_from_index_mode(&mut self) {
        let mode = self.index_mode.as_str();
        let (precision, state, work) = match mode {
            "full" => (
                "repo_complete".to_string(),
                "ready".to_string(),
                String::new(),
            ),
            "structural" => (
                "local_complete (0.7)".to_string(),
                "usable_partial".to_string(),
                "focus available".to_string(),
            ),
            "manifest" => (
                "manifest (low)".to_string(),
                "usable_partial".to_string(),
                "run structural or search to refine".to_string(),
            ),
            "partial" | "empty" | "none" | "unknown" => (
                "unavailable".to_string(),
                "blocked".to_string(),
                "index required".to_string(),
            ),
            _ => (mode.to_string(), "ready".to_string(), String::new()),
        };

        self.analysis_hud = AnalysisHud {
            precision,
            work,
            gap_count: if mode == "full" { 0 } else { 1 },
            state,
        };
    }

    /// Called from render (and can be called after jobs that carry focus data).
    /// Currently returns the cached hud (enriched in refresh or job handlers later).
    fn compute_basic_hud(&self) -> AnalysisHud {
        self.analysis_hud.clone()
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

fn render_help_popup(frame: &mut Frame, area: Rect) {
    // Compact cheatsheet popup for hybrid multi-tool + focus keys.
    // Self-adversarial: popup (like exit) chosen over permanent footer (space) or bottom bar (clutter).
    // Shows the i/v/x + t + focus state visibility + nav without requiring README dive.
    let popup = centered_in(area, 54, 14); // 帮助内容按 plan 扩展为模式解释 + 流程示例
    frame.render_widget(Clear, popup);

    // 按已批准中文 plan 重写 help：必须解释显式 Query/Browse 模式 + “其他搜索模式”如何触发 + 示例流程。
    // 这是 discoverability 的关键 affordance。
    let lines = vec![
        "  Atlas TUI 按键模式（? 切换本帮助）                          ",
        "  核心：Querying（打字） vs Browsing（列表存在时默认，工具可用）",
        "  有结果列表时 Browsing：i=Impact(MCP) v=VarTrace x=clear   ",
        "  j/k / arrows / Pg 导航选择；Enter 打开详情或默认动作       ",
        "  / 或 s 进 Querying（可 refine，保留列表；非保留字母输入）  ",
        "  Esc 逐级返回/清除；? 帮助；Ctrl-C 退出                    ",
        "  示例流程：输入 query Enter → 自动 Browsing → j/k 选 → i 做 Impact",
        "    → 右下工具结果 bar + HUD 'local (tool)' + focus 状态更新   ",
        "    → 可立即 v 或换选择再 i；x 清除 overlay；/ 回 Query refine",
        "  focus HUD (P/w/g/[state]) + status MODE: 一直可见 partial/tool",
        "  其他屏幕：/ 跳新搜索；Esc 返回；t/i/v/x 在 detail 也可用     ",
    ];
    let p = Paragraph::new(lines.join("\n"))
        .block(Block::default().borders(Borders::ALL).title(" Help "))
        .style(Style::default().fg(Color::White));
    frame.render_widget(p, popup);
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
            analysis_hud: AnalysisHud::default(),
            last_tool_result: None,
            current_tool: ToolKind::None,
            help_visible: false,
            interaction_mode: InteractionMode::Querying,
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
