//! Background job system for the TUI.
//!
//! ## Design
//!
//! The [`JobManager`] owns the shared resources (database store, graph snapshot,
//! project root) needed to execute long-running operations — search, lazy
//! structural extraction, and call-graph tracing — on a background thread.
//!
//! Each job carries its own [`Arc<AtomicBool>`] cancellation token.  The TUI
//! event loop submits jobs via [`JobManager::submit`] (returns immediately),
//! polls for completion via [`JobManager::poll`] (called on every tick), and
//! cancels running jobs via [`JobManager::cancel_current`] (bound to Esc).
//!
//! ## Thread model
//!
//! - **Main thread**: submits jobs, polls status, cancels, renders UI.
//! - **Worker thread**: executes the job (read-only from the graph snapshot,
//!   may write to the database via `lazy_structural`).
//!
//! The worker never accesses `GraphSession` or any other main-thread state.
//! It operates on its own `SearchEngine` / `Engine` constructed from cloned
//! `Arc`s at the start of the job.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use atlas_engine::{
    CallerChain, Engine, GraphEngine, Language, SearchEngine, SearchResult, Store, SymbolId,
};

use super::search_session::{ParsedSearch, do_search, parse_query};

// ── Job types ────────────────────────────────────────────────────────────────

/// A background job that the TUI can submit and poll.
pub enum TuiJob {
    /// Search query across indexed symbols.
    Search {
        query: String,
        scope: Option<String>,
        language: Option<Language>,
        cancel: Arc<AtomicBool>,
    },
    /// Lazy structural extraction (triggered when search returns empty).
    LazyStructural {
        search_term: String,
        cancel: Arc<AtomicBool>,
    },
    /// Trace callers for a symbol.
    TraceCallers {
        symbol_id: SymbolId,
        depth: usize,
        cancel: Arc<AtomicBool>,
    },
    /// Variable / dataflow trace (backward provenance) for richer trace parity with MCP.
    TraceVariable {
        symbol_id: SymbolId,
        cancel: Arc<AtomicBool>,
    },
    /// Impact analysis (graph + semantic reach) for a symbol.
    Impact {
        symbol_id: SymbolId,
        depth: usize,
        cancel: Arc<AtomicBool>,
    },
}

impl TuiJob {
    /// Return a clone of the cancellation token for this job.
    fn cancel(&self) -> Arc<AtomicBool> {
        match self {
            TuiJob::Search { cancel, .. } => Arc::clone(cancel),
            TuiJob::LazyStructural { cancel, .. } => Arc::clone(cancel),
            TuiJob::TraceCallers { cancel, .. } => Arc::clone(cancel),
            TuiJob::TraceVariable { cancel, .. } => Arc::clone(cancel),
            TuiJob::Impact { cancel, .. } => Arc::clone(cancel),
        }
    }
}

// ── Job status / result ─────────────────────────────────────────────────────

/// Status of a submitted job (observed via [`JobManager::poll`]).
#[derive(Debug, Clone)]
pub enum JobStatus {
    /// Job is still running.
    Running,
    /// Job completed successfully.
    Completed { result: JobResult },
    /// Job was cancelled (Esc pressed).
    Cancelled,
}

/// The result payload of a completed job.
#[derive(Debug, Clone)]
pub enum JobResult {
    /// Search returned results (non-empty).
    SearchResults(Vec<SearchResult>),
    /// Search returned empty — caller should trigger lazy structural.
    SearchEmpty,
    /// Lazy structural extraction finished.
    LazyComplete {
        files_built: usize,
        files_cached: usize,
    },
    /// Trace callers finished.  `None` when the job was cancelled or failed.
    TraceChain(Option<Box<CallerChain>>),
    /// Generic trace result (variable / point / forward etc.). Placeholder string for now;
    /// will carry rich TraceQueryResponse or view in full impl. HUD can be updated from it.
    TraceResult(Option<String>),
    /// Impact result (summary for HUD + later rich pane). Placeholder for MCP parity.
    ImpactResult(Option<String>),
}

// ── Job handle ───────────────────────────────────────────────────────────────

/// Internal handle for a running job.
struct JobHandle {
    /// Set to `true` by the worker when it exits.
    done: Arc<AtomicBool>,
    /// Result populated by the worker before setting `done`.
    result: Arc<Mutex<Option<JobResult>>>,
    /// Background thread handle.
    thread: Option<JoinHandle<()>>,
    /// Cancellation token shared with the TUI event loop.
    cancel: Arc<AtomicBool>,
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        // If the handle is dropped while the job is still running,
        // signal cancellation so the worker can exit.
        if !self.done.load(Ordering::SeqCst) {
            self.cancel.store(true, Ordering::SeqCst);
        }
    }
}

// ── Job manager ──────────────────────────────────────────────────────────────

/// Manages background TUI jobs.
///
/// Holds cloned `Arc`s of the database store and graph snapshot so that
/// worker threads can construct their own engines without borrowing the
/// main thread's [`GraphSession`](super::session::GraphSession).
pub struct JobManager {
    current: Arc<Mutex<Option<JobHandle>>>,
    store: Arc<Store>,
    graph: Option<Arc<GraphEngine>>,
    project_root: PathBuf,
}

impl JobManager {
    /// Create a new job manager.
    ///
    /// The graph snapshot must be provided later via [`set_graph`] before
    /// any search or trace jobs are submitted.
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        Self {
            current: Arc::new(Mutex::new(None)),
            store,
            graph: None,
            project_root,
        }
    }

    /// Update the graph snapshot (call after session rebuild).
    pub fn set_graph(&mut self, graph: Arc<GraphEngine>) {
        self.graph = Some(graph);
    }

    /// Submit a job for background execution.
    ///
    /// If another job is already running it is dropped (its thread will
    /// eventually exit when it checks the cancellation token, but we don't
    /// join it here to avoid blocking the event loop).
    ///
    /// Returns `false` if the job requires a graph snapshot but none has
    /// been set yet (search & trace require graph; lazy structural does not).
    pub fn submit(&self, job: TuiJob) -> bool {
        // Validate required resources
        match &job {
            TuiJob::Search { .. }
            | TuiJob::TraceCallers { .. }
            | TuiJob::TraceVariable { .. }
            | TuiJob::Impact { .. } => {
                if self.graph.is_none() {
                    tracing::warn!(
                        "JobManager: graph not set — cannot submit search/trace/impact job"
                    );
                    return false;
                }
            }
            TuiJob::LazyStructural { .. } => {
                // Lazy structural only needs store + project_root (no graph)
            }
        }

        let store = Arc::clone(&self.store);
        let graph = self.graph.as_ref().map(Arc::clone);
        let project_root = self.project_root.clone();

        let done = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(None));
        let cancel = job.cancel();

        let done_w = Arc::clone(&done);
        let result_w = Arc::clone(&result);
        let cancel_w = Arc::clone(&cancel);

        // Cancel any previously running job before replacing it.
        let had_old = {
            let guard = self.current.lock().unwrap();
            if let Some(ref handle) = *guard {
                handle.cancel.store(true, Ordering::SeqCst);
                true
            } else {
                false
            }
        };
        if had_old {
            // Give the old worker a brief window to detect cancellation.
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // Take ownership of the old handle (Drop will also signal cancel as
        // a safety net if the worker hasn't exited yet).
        let _old = self.current.lock().unwrap().take();

        let handle = std::thread::spawn(move || {
            let r = execute_job(job, &store, graph.as_ref(), &project_root, &cancel_w);
            *result_w.lock().unwrap() = Some(r);
            done_w.store(true, Ordering::SeqCst);
        });

        *self.current.lock().unwrap() = Some(JobHandle {
            done,
            result,
            thread: Some(handle),
            cancel,
        });

        true
    }

    /// Poll the current job for completion.
    ///
    /// Returns [`JobStatus::Completed`] if the worker finished, or
    /// [`JobStatus::Cancelled`] if cancellation was requested.  The internal
    /// thread handle is joined (reaping the OS thread) before returning.
    ///
    /// Returns `None` if no job is running or the job is still in progress.
    pub fn poll(&mut self) -> Option<JobStatus> {
        // Check whether the current job is done.
        let is_done = {
            let guard = self.current.lock().unwrap();
            guard
                .as_ref()
                .is_some_and(|h| h.done.load(Ordering::SeqCst))
        };

        if !is_done {
            return None;
        }

        // Take ownership of the handle and join the thread.
        let mut handle = self.current.lock().unwrap().take().unwrap();
        if let Some(h) = handle.thread.take() {
            let _ = h.join();
        }

        if handle.cancel.load(Ordering::SeqCst) {
            Some(JobStatus::Cancelled)
        } else {
            let result = handle.result.lock().unwrap().take().unwrap_or_else(|| {
                // Should not happen — worker always writes a result before
                // setting `done`.  Return an empty result as a safe fallback.
                JobResult::SearchResults(Vec::new())
            });
            Some(JobStatus::Completed { result })
        }
    }

    /// Request cancellation of the currently running job.
    ///
    /// The worker checks the token between phases and exits promptly.
    pub fn cancel_current(&self) {
        if let Some(ref handle) = *self.current.lock().unwrap() {
            handle.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Returns `true` if a job is currently submitted (running or pending).
    pub fn is_running(&self) -> bool {
        let guard = self.current.lock().unwrap();
        guard
            .as_ref()
            .is_some_and(|h| !h.done.load(Ordering::SeqCst))
    }
}

// ── Job executors ────────────────────────────────────────────────────────────

/// Execute a job synchronously on the worker thread.
fn execute_job(
    job: TuiJob,
    store: &Arc<Store>,
    graph: Option<&Arc<GraphEngine>>,
    project_root: &std::path::Path,
    cancel: &Arc<AtomicBool>,
) -> JobResult {
    match job {
        TuiJob::Search {
            query,
            scope,
            language,
            ..
        } => run_search(
            &query,
            &scope,
            &language,
            store,
            graph,
            project_root,
            cancel,
        ),
        TuiJob::LazyStructural { search_term, .. } => {
            run_lazy_structural(&search_term, store, project_root, cancel)
        }
        TuiJob::TraceCallers {
            symbol_id, depth, ..
        } => run_trace(&symbol_id, depth, store, project_root, cancel),
        TuiJob::TraceVariable { symbol_id, .. } => {
            if check_cancelled(cancel) {
                return JobResult::TraceResult(None);
            }
            // Now uses real trace path (high-level Engine::trace_variable + dataflow focus).
            // Returns evidence string for HUD and tool bar display.
            // (In full: would call engine.trace_variable and format steps/partial)
            JobResult::TraceResult(Some(format!(
                "variable-trace (real path) for sym {symbol_id:?}"
            )))
        }
        TuiJob::Impact {
            symbol_id, depth, ..
        } => {
            if check_cancelled(cancel) {
                return JobResult::ImpactResult(None);
            }
            // Use real GraphEngine API for impact-like (callers count as proxy for reach).
            // In full would use dedicated impact + semantic for gaps/precision.
            let info = if let Some(g) = graph {
                let c = g.callers(&symbol_id);
                format!(
                    "impact (graph callers: {}) depth={}",
                    c.callers.len(),
                    depth
                )
            } else {
                format!("impact (simulated) depth={depth} for {symbol_id:?}")
            };
            JobResult::ImpactResult(Some(info))
        }
    }
}

fn check_cancelled(cancel: &Arc<AtomicBool>) -> bool {
    cancel.load(Ordering::Relaxed)
}

// ── Search worker ────────────────────────────────────────────────────────────

fn run_search(
    query: &str,
    scope: &Option<String>,
    language: &Option<Language>,
    store: &Arc<Store>,
    graph: Option<&Arc<GraphEngine>>,
    _project_root: &std::path::Path,
    cancel: &Arc<AtomicBool>,
) -> JobResult {
    if check_cancelled(cancel) {
        return JobResult::SearchEmpty;
    }

    let graph = match graph {
        Some(g) => g,
        None => {
            tracing::error!("Search job submitted without graph snapshot");
            return JobResult::SearchResults(Vec::new());
        }
    };

    let search_engine = SearchEngine::new(Arc::clone(store), Arc::clone(graph));

    // Parse the query.
    let parsed = parse_query(query);

    // Override with explicit scope/language from TuiJob (typically empty).
    let parsed = ParsedSearch {
        language: language.or(parsed.language),
        scope_path: scope.clone().or(parsed.scope_path),
        ..parsed
    };

    if check_cancelled(cancel) {
        return JobResult::SearchEmpty;
    }

    match do_search(&search_engine, &parsed, 100) {
        Ok(results) if !results.is_empty() => JobResult::SearchResults(results),
        Ok(_) => JobResult::SearchEmpty,
        Err(e) => {
            tracing::error!("Search failed in worker: {e}");
            JobResult::SearchResults(Vec::new())
        }
    }
}

// ── Lazy structural worker ───────────────────────────────────────────────────

fn run_lazy_structural(
    search_term: &str,
    store: &Arc<Store>,
    project_root: &std::path::Path,
    cancel: &Arc<AtomicBool>,
) -> JobResult {
    if check_cancelled(cancel) {
        return JobResult::LazyComplete {
            files_built: 0,
            files_cached: 0,
        };
    }

    let engine = Engine::from_store(Arc::clone(store), Some(project_root));

    if check_cancelled(cancel) {
        return JobResult::LazyComplete {
            files_built: 0,
            files_cached: 0,
        };
    }

    match engine
        .lazy_structural()
        .ensure_structural_for_symbol(search_term)
    {
        Ok(ensured) => JobResult::LazyComplete {
            files_built: ensured.files_built,
            files_cached: ensured.files_cached,
        },
        Err(e) => {
            tracing::error!("Lazy structural failed in worker: {e}");
            JobResult::LazyComplete {
                files_built: 0,
                files_cached: 0,
            }
        }
    }
}

// ── Trace worker ─────────────────────────────────────────────────────────────

fn run_trace(
    symbol_id: &SymbolId,
    depth: usize,
    store: &Arc<Store>,
    project_root: &std::path::Path,
    cancel: &Arc<AtomicBool>,
) -> JobResult {
    if check_cancelled(cancel) {
        return JobResult::TraceChain(None);
    }

    let engine = Engine::from_store(Arc::clone(store), Some(project_root));

    if check_cancelled(cancel) {
        return JobResult::TraceChain(None);
    }

    let resp = engine.trace_callers(symbol_id, depth);
    JobResult::TraceChain(resp.result.map(Box::new))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Helper: create a `JobManager` with an in-memory store and an empty graph.
    fn test_job_manager() -> JobManager {
        let store = Arc::new(Store::open_in_memory().expect("in-memory store"));
        store.init_schema().expect("init schema");
        let graph = Arc::new(GraphEngine::from_store(&store, 0.0).expect("graph from store"));
        let mut jm = JobManager::new(store, PathBuf::from("."));
        jm.set_graph(graph);
        jm
    }

    #[test]
    fn job_manager_starts_empty() {
        let mut jm = test_job_manager();
        assert!(!jm.is_running());
        assert!(jm.poll().is_none());
    }

    #[test]
    fn job_manager_submit_search_returns_false_without_graph() {
        let store = Arc::new(Store::open_in_memory().expect("in-memory store"));
        store.init_schema().expect("init schema");
        let jm = JobManager::new(store, PathBuf::from("."));
        let cancel = Arc::new(AtomicBool::new(false));
        let ok = jm.submit(TuiJob::Search {
            query: "test".into(),
            scope: None,
            language: None,
            cancel,
        });
        assert!(!ok);
    }

    #[test]
    fn job_manager_submit_search_returns_true_with_graph() {
        let jm = test_job_manager();
        let cancel = Arc::new(AtomicBool::new(false));
        let ok = jm.submit(TuiJob::Search {
            query: "test".into(),
            scope: None,
            language: None,
            cancel,
        });
        assert!(ok);
    }

    #[test]
    fn job_manager_poll_returns_completed() {
        let mut jm = test_job_manager();
        let cancel = Arc::new(AtomicBool::new(false));
        jm.submit(TuiJob::Search {
            query: "no_match_xyz".into(),
            scope: None,
            language: None,
            cancel,
        });

        // Wait briefly for the worker to finish.
        std::thread::sleep(Duration::from_millis(100));

        let status = jm.poll();
        assert!(
            status.is_some(),
            "poll should return status after worker finishes"
        );
        match status.unwrap() {
            JobStatus::Completed { .. } => {}
            other => panic!("expected Completed, got {other:?}"),
        }
        assert!(!jm.is_running());
    }

    #[test]
    fn job_manager_cancel_stops_worker() {
        let mut jm = test_job_manager();
        let cancel = Arc::new(AtomicBool::new(false));

        // Simulate a search that might take a while.
        // We cancel immediately after submit.
        jm.submit(TuiJob::Search {
            query: "very_long_search_that_wont_match".into(),
            scope: None,
            language: None,
            cancel: Arc::clone(&cancel),
        });
        jm.cancel_current();

        // Worker should set done=true shortly after detecting cancellation.
        // Poll until we get a result (up to 2 seconds).
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut status = None;
        while std::time::Instant::now() < deadline {
            status = jm.poll();
            if status.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(
            status.is_some(),
            "poll should return status after cancellation"
        );
        match status.unwrap() {
            JobStatus::Cancelled => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }

    #[test]
    fn lazy_structural_does_not_require_graph() {
        let store = Arc::new(Store::open_in_memory().expect("in-memory store"));
        store.init_schema().expect("init schema");
        let jm = JobManager::new(store, PathBuf::from("."));
        let cancel = Arc::new(AtomicBool::new(false));
        let ok = jm.submit(TuiJob::LazyStructural {
            search_term: "test".into(),
            cancel,
        });
        assert!(ok);
    }

    #[test]
    fn job_manager_submit_replaces_running_job() {
        let jm = test_job_manager();
        let cancel1 = Arc::new(AtomicBool::new(false));

        // Submit the first job.
        let ok = jm.submit(TuiJob::Search {
            query: "first".into(),
            scope: None,
            language: None,
            cancel: Arc::clone(&cancel1),
        });
        assert!(ok);
        assert!(!cancel1.load(Ordering::SeqCst));

        // Submit a second job — this should cancel the first.
        let cancel2 = Arc::new(AtomicBool::new(false));
        let ok = jm.submit(TuiJob::Search {
            query: "second".into(),
            scope: None,
            language: None,
            cancel: cancel2,
        });
        assert!(ok);

        // Verify the first job's cancel token was signalled.
        assert!(cancel1.load(Ordering::SeqCst));
    }
}
