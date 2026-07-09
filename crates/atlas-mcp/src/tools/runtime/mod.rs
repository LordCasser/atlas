//! Runtime modules — the v6.0 architecture backbone.
//!
//! Each runtime module wraps a specific concern that was previously
//! crammed into the 15-field ToolRouter god object:
//!
//! | Module | Concern | Owns |
//! |--------|---------|------|
//! | `query_runtime` | Focus-driven lazy extraction | FocusRuntime, CacheState, LazyRefreshQueue |
//! | `graph_runtime` | In-memory call-graph snapshot | GraphState, SearchEngine, ContextBuilder, GraphProvider |
//! | `analysis_runtime` | CFG/dataflow ensure + analysis dispatch | LazyDataflowService, lifecycle/branch_diff orchestration |
//! | `overlay_runtime` | User annotations (fp_dispatches, domain_rules) | Store (mutation path), RuntimeInvalidation counters |
//! | `store_query_runtime` | Direct store queries + source extraction | Store (read path), SourceExtractor |
//! | `job_runtime` | Per-project investigation + query snapshots | InvestigationState, QuerySnapshot map |
//! | `cache_state` | Index-signature and manual-full-index caching | (data-only) |
//! | `graph_provider` | Trait contract for graph backends | (trait definition) |
//!
//! # Request Flow
//!
//! ```text
//!   MCP Client
//!       │
//!       ▼
//!   AtlasMcpService (lib.rs)
//!       │  lock_router()
//!       ▼
//!   ToolRouter.call_tool(ctx, name, args)
//!       │
//!       ├─ contract_for(name, args) → ToolContract
//!       │
//!       ├─ [SemanticGraphQuery/TraceQuery]:
//!       │    ensure_graph_initialized()  ───►  graph_runtime
//!       │    maybe_refresh_graph()      ───►  graph_runtime + query_runtime.cache
//!       │
//!       ├─ match ToolContract → sub-dispatcher
//!       │
//!       ├─ Handler (graph.rs, search.rs, …)
//!       │    │
//!       │    ├─ prepare_focus_query(intent, include_roots) ──► query_runtime.prepare()
//!       │    │       │                           │
//!       │    │       │                           ├─ cache_state.has_full_index()
//!       │    │       │                           ├─ focus_runtime.lock().detect_access_strategy()
//!       │    │       │                           └─ focus_runtime.lock().prepare(intent, include_roots)
//!       │    │       │
//!       │    │       └─ Post: lazy_refresh_queue.record_writes()
//!       │    │                 maybe_refresh_graph()
//!       │    │
//!       │    ├─ context_builder()  ──►  graph_runtime → GraphSnapshot
//!       │    ├─ search_engine()    ──►  graph_runtime → GraphSnapshot
//!       │    ├─ resolve_file_path() ──►  store_query_runtime
//!       │    ├─ read_symbol_source()──►  store_query_runtime
//!       │    └─ engine.lock()      ──►  active.engine (trace only)
//!       │
//!       ▼
//!   CallToolResult (JSON)
//! ```
//!
//! # Contract → Runtime Mapping
//!
//! | Contract | Runtimes Involved |
//! |----------|-------------------|
//! | `ProjectLifecycle` | job_runtime, graph_runtime (reset) |
//! | `StatusRead` | store_query_runtime (stats queries) |
//! | `SemanticGraphQuery` | graph_runtime (snapshot), query_runtime (focus), store_query_runtime (source) |
//! | `TraceQuery` | engine (direct, not graph_runtime), query_runtime (focus), store_query_runtime |
//! | `StoreFactQuery` | query_runtime (focus, +graph for symbol context), store_query_runtime |
//! | `SemanticAnalysis` | analysis_runtime (CFG/dataflow), store_query_runtime |
//! | `OverlayMutation` | overlay_runtime (mutation + generation counter) |
//! | `OverlayRead` | store_query_runtime (read-only) |
//! | `TaskControl` | store_query_runtime (extraction job observability), job_runtime (query snapshots) |
//!
//! # Concurrency Model
//!
//! - **ToolRouter** is immutable orchestration state over the active project.
//! - **engine** (`Mutex<Engine>`) is only accessed from trace handlers — held briefly.
//! - **focus_runtime** (`Mutex<FocusRuntime>`) serializes foreground closure preparation per
//!   active project. Other store reads and graph snapshot queries remain concurrent.
//! - **graph_runtime.state** holds a `RwLock<Arc<GraphEngine>>` — readers share the snapshot.
//! - **RuntimeInvalidation** counters (`AtomicU64`) are lock-free for fast-path invalidation.
//! - **Background tasks** (graph rebuild, focus scheduler) use `std::thread::spawn`
//!   with cloned `Arc<Store>` — they never access ToolRouter directly.
//!
//! # Anti-Patterns (enforced by `handler_purity` tests — DEBT-8 ratchet)
//!
//! Handlers produce contract data; orchestration belongs to `call_tool` /
//! `dispatch_*` + runtime modules. When adding tools or editing handlers, **avoid**:
//!
//! - **Direct `cache.has_manual_full_index()`** — use `query_runtime.has_full_index()`.
//! - **Direct `focus_runtime.lock()`** — use `query_runtime.prepare()` / `detect_access_strategy()`.
//! - **Direct `materialize.dataflow().ensure_*`** — use `analysis_runtime.ensure_dataflow_*`.
//! - **Direct `FieldLifecycleEngine::` / `BranchDiffEngine::`** — go through
//!   `analysis_runtime.run_lifecycle` / `run_branch_diff` (or semantic helpers).
//! - **Direct `store.upsert_fp_annotation()` / `upsert_domain_rule()`** — use `overlay_runtime`.
//! - **Direct `graph_state.ensure_initialized()`** — use `graph_runtime.ensure_initialized()`.
//! - **Direct `store` path resolve** — use `store_query_runtime.resolve_file_path()`.
//! - **Adding fields to ToolRouter** — add to the appropriate runtime module.
//!
//! Migration: shrink `handler_purity::ALLOWLIST` as tools move fully onto dispatch.

pub(crate) mod analysis_runtime;
pub(crate) mod cache_state;
pub(crate) mod closure_graph_provider;
pub(crate) mod graph_provider;
pub(crate) mod graph_runtime;
pub(crate) mod graph_state;
pub(crate) mod invalidation;
pub(crate) mod job_runtime;
pub(crate) mod overlay_runtime;
pub(crate) mod query_runtime;
pub(crate) mod store_query_runtime;
